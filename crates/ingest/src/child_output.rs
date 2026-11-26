//! Forwarding of the child's standard output and standard error to the log
//! (ADR 0001, "Logging").
//!
//! Upstream logs a VIN on some error paths, so each line of the child goes
//! through the redaction below before it reaches the log.
//!
//! - **Lines.** A line ends at `\n`. The assembler keeps at most
//!   [`MAX_LINE`] bytes (8 KiB) of a line. It drops the rest of a longer
//!   line up to the next `\n` and marks the line as truncated. A last line
//!   without `\n` is forwarded at the end of the stream.
//! - **VIN-shaped tokens.** ADR 0001 defines a VIN-shaped token as 17
//!   characters from `[A-HJ-NPR-Z0-9]`. The redaction replaces each maximal
//!   run of 17 or more such characters with `[vin]`. A VIN can stand next to
//!   other characters of the same set, for example after a digit, so the
//!   whole run goes, not only 17 characters of it. Shorter runs stay.
//!   Lowercase letters are not in the set.
//! - **Truncated lines.** The cut of a truncated line can split a VIN. A
//!   run of VIN characters at the cut is replaced too, whatever its length.
//! - **Bytes that are not UTF-8.** The redaction works on bytes before the
//!   conversion to text. The VIN characters are ASCII, and no byte of a
//!   multi-byte character is ASCII, so such a character cannot hide a VIN.
//!   After the redaction, bytes that are not valid UTF-8 become U+FFFD.
//!
//! Only [`redact`] makes a [`RedactedLine`], and the log gets only the text
//! of that type, in the field `child_line`.
//!
//! Bounds: each stream uses one read buffer of 8 KiB and one line buffer of
//! at most 8 KiB.

use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::{info, warn};

/// The longest line that the log gets, in bytes.
pub const MAX_LINE: usize = 8 * 1024;

/// Size of the read buffer of each stream.
const READ_CHUNK: usize = 8 * 1024;

/// The length of a VIN.
const VIN_LEN: usize = 17;

/// The replacement of a VIN-shaped token.
const VIN_MARK: &[u8] = b"[vin]";

/// A line after the redaction. Only [`redact`] makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedLine {
    text: String,
    truncated: bool,
}

impl RedactedLine {
    /// The text of the line without VIN-shaped tokens.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// True when the line was longer than [`MAX_LINE`].
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Replaces the VIN-shaped tokens of one line (module documentation).
/// `truncated` tells that the line was cut after its last byte.
#[must_use]
pub fn redact(line: &[u8], truncated: bool) -> RedactedLine {
    let mut redacted = Vec::with_capacity(line.len());
    let mut rest = line;
    while let Some(&first) = rest.first() {
        if is_vin_char(first) {
            let run = rest.iter().take_while(|&&byte| is_vin_char(byte)).count();
            let (token, after) = rest.split_at(run);
            let at_cut = truncated && after.is_empty();
            if run >= VIN_LEN || at_cut {
                redacted.extend_from_slice(VIN_MARK);
            } else {
                redacted.extend_from_slice(token);
            }
            rest = after;
        } else {
            let other = rest.iter().take_while(|&&byte| !is_vin_char(byte)).count();
            let (text, after) = rest.split_at(other);
            redacted.extend_from_slice(text);
            rest = after;
        }
    }
    RedactedLine {
        text: String::from_utf8_lossy(&redacted).into_owned(),
        truncated,
    }
}

/// A character of the VIN alphabet: digits and capital letters without I,
/// O, and Q.
const fn is_vin_char(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'Z')
}

/// Builds lines from chunks with a bounded buffer.
#[derive(Debug, Default)]
struct LineAssembler {
    line: Vec<u8>,
    truncated: bool,
}

impl LineAssembler {
    /// Adds bytes and gives each completed line to `emit`.
    fn push(&mut self, mut bytes: &[u8], emit: &mut impl FnMut(RedactedLine)) {
        while !bytes.is_empty() {
            let (part, rest, ended) = match bytes.iter().position(|&byte| byte == b'\n') {
                Some(end) => {
                    let (part, rest) = bytes.split_at(end);
                    (part, rest.get(1..).unwrap_or_default(), true)
                }
                None => (bytes, &[][..], false),
            };
            let room = MAX_LINE.saturating_sub(self.line.len());
            if part.len() > room {
                self.line
                    .extend_from_slice(part.get(..room).unwrap_or_default());
                self.truncated = true;
            } else {
                self.line.extend_from_slice(part);
            }
            if ended {
                self.emit(emit);
            }
            bytes = rest;
        }
    }

    /// Gives the last line without `\n`, if one exists, to `emit`.
    fn finish(&mut self, emit: &mut impl FnMut(RedactedLine)) {
        if !self.line.is_empty() || self.truncated {
            self.emit(emit);
        }
    }

    fn emit(&mut self, emit: &mut impl FnMut(RedactedLine)) {
        emit(redact(&self.line, self.truncated));
        self.line.clear();
        self.truncated = false;
    }
}

/// Reads one output stream of the child until its end and logs each line
/// as the event `child_output`. `stream` is `stdout` or `stderr`.
pub async fn forward(mut reader: impl AsyncRead + Unpin, stream: &'static str) {
    let mut chunk = vec![0_u8; READ_CHUNK];
    let mut lines = LineAssembler::default();
    let mut log = |line: RedactedLine| {
        info!(
            stream,
            child_line = line.text(),
            truncated = line.truncated(),
            "child_output"
        );
    };
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => lines.push(chunk.get(..read).unwrap_or_default(), &mut log),
            Err(error) => {
                warn!(stream, error = %error, "child_output_failed");
                break;
            }
        }
    }
    lines.finish(&mut log);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{ALLOWED_FIELDS, capture};

    /// An invented VIN-shaped token. It is not a real VIN.
    const FAKE_VIN: &str = "0FAKEVXN000000001";

    fn text(line: &str) -> String {
        redact(line.as_bytes(), false).text
    }

    #[test]
    fn replaces_a_vin_shaped_token() {
        assert_eq!(FAKE_VIN.len(), 17);
        assert_eq!(
            text(&format!("vin={FAKE_VIN} connected")),
            "vin=[vin] connected"
        );
        assert_eq!(
            text(&format!("{{\"vin\":\"{FAKE_VIN}\",\"x\":1}}")),
            "{\"vin\":\"[vin]\",\"x\":1}"
        );
        assert_eq!(text(FAKE_VIN), "[vin]");
        assert_eq!(text(&format!("{FAKE_VIN}{FAKE_VIN}")), "[vin]");
    }

    #[test]
    fn replaces_a_vin_inside_a_longer_run_of_vin_characters() {
        // A VIN after a digit or a capital letter is one run of 18.
        assert_eq!(text(&format!("id9{FAKE_VIN}")), "id[vin]");
        assert_eq!(text(&format!("{FAKE_VIN}ZZ end")), "[vin] end");
    }

    #[test]
    fn keeps_shorter_runs_and_characters_outside_the_set() {
        let short = &FAKE_VIN[..16];
        assert_eq!(text(short), short);
        // I, O, and Q are not VIN characters, and neither are lowercase
        // letters.
        let with_i = "0FAKEVIN000000001";
        assert_eq!(text(with_i), with_i);
        let lower = FAKE_VIN.to_lowercase();
        assert_eq!(text(&lower), lower);
        assert_eq!(
            text("level=info msg=\"listening\" port=4444"),
            "level=info msg=\"listening\" port=4444"
        );
    }

    #[test]
    fn replaces_a_run_at_the_cut_of_a_truncated_line() {
        let line = redact(b"prefix 0FAKEV", true);
        assert_eq!(line.text(), "prefix [vin]");
        assert!(line.truncated());
        // Without a cut, the short run stays.
        assert_eq!(text("prefix 0FAKEV"), "prefix 0FAKEV");
    }

    #[test]
    fn replaces_a_vin_next_to_bytes_that_are_not_utf8() {
        let mut bytes = vec![0xff, 0xc3];
        bytes.extend_from_slice(FAKE_VIN.as_bytes());
        bytes.push(0x80);
        let line = redact(&bytes, false);
        assert_eq!(line.text(), "\u{fffd}\u{fffd}[vin]\u{fffd}");
        // A multi-byte character stays.
        assert_eq!(text("tür ok"), "tür ok");
    }

    fn assemble(chunks: &[&[u8]]) -> Vec<RedactedLine> {
        let mut lines = Vec::new();
        let mut assembler = LineAssembler::default();
        let mut emit = |line| lines.push(line);
        for chunk in chunks {
            assembler.push(chunk, &mut emit);
        }
        assembler.finish(&mut emit);
        lines
    }

    #[test]
    fn assembles_a_vin_that_is_split_across_reads() {
        let lines = assemble(&[b"one 0FAKEVXN", b"000000001 two\nthree", b"\n"]);
        let texts: Vec<&str> = lines.iter().map(RedactedLine::text).collect();
        assert_eq!(texts, ["one [vin] two", "three"]);
    }

    #[test]
    fn forwards_a_last_line_without_a_newline() {
        let lines = assemble(&[b"first\nlast"]);
        let texts: Vec<&str> = lines.iter().map(RedactedLine::text).collect();
        assert_eq!(texts, ["first", "last"]);
        assert_eq!(assemble(&[b"a\n"]).len(), 1);
        assert_eq!(assemble(&[b"\n"])[0].text(), "");
    }

    #[test]
    fn truncates_a_long_line_at_8_kib_and_keeps_the_next_line() {
        let exact = vec![b'a'; MAX_LINE];
        let lines = assemble(&[&exact, b"\n"]);
        assert_eq!(lines[0].text().len(), MAX_LINE);
        assert!(!lines[0].truncated());

        let long = vec![b'a'; MAX_LINE * 3 + 5];
        let (head, tail) = long.split_at(MAX_LINE + 1);
        let lines = assemble(&[head, tail, b"\nnext\n"]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text().len(), MAX_LINE);
        assert!(lines[0].truncated());
        assert_eq!(lines[1].text(), "next");
        assert!(!lines[1].truncated());
    }

    #[test]
    fn a_vin_across_the_8_kib_cut_is_replaced() {
        let mut line = vec![b'a'; MAX_LINE - 10];
        line.extend_from_slice(FAKE_VIN.as_bytes());
        line.extend_from_slice(b" tail\n");
        let lines = assemble(&[&line]);
        assert!(lines[0].truncated());
        assert!(lines[0].text().ends_with("a[vin]"), "{}", lines[0].text());
        assert!(!lines[0].text().contains("0FAKEVXN00"));
    }

    #[tokio::test]
    async fn forward_logs_each_redacted_line() {
        let (capture, _guard) = capture::install(ALLOWED_FIELDS);
        let input = format!("vin {FAKE_VIN}\nplain line\n");
        forward(input.as_bytes(), "stderr").await;
        let text = capture.text();
        assert!(!text.contains(FAKE_VIN), "{text}");
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["message"], "child_output");
        assert_eq!(events[0]["stream"], "stderr");
        assert_eq!(events[0]["child_line"], "vin [vin]");
        assert_eq!(events[0]["truncated"], false);
        assert_eq!(events[1]["child_line"], "plain line");
    }
}
