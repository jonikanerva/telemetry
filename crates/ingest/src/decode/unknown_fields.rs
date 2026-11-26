//! Finds fields that the vendored schema does not know.
//!
//! `prost` skips an unknown field without a trace. The upstream server
//! keeps unknown fields: it decodes and encodes each payload again with
//! the Go defaults, `proto.Unmarshal` and `proto.Marshal` without options
//! (`telemetry/record.go`, lines 152-201), and the Go library keeps the
//! unknown fields of a message and writes them again. So a vehicle with a
//! newer schema than the vendored one can send a field that reaches the
//! ingest process. This module finds such a field, so that the storage
//! keeps the original bytes (reason `unknown_fields`, ADR 0002).
//!
//! The check reads the wire format and compares each field number with the
//! field numbers of its message type in the vendored schema. `build.rs`
//! writes that table from the `.proto` files. The check does not compare
//! lengths or bytes, so the encoding conventions of the sender (field
//! order, packed or unpacked repeated fields, map entries with default
//! keys or values) cannot cause a false result.
//!
//! The check runs only after `prost` has decoded the same bytes, so the
//! wire format is valid. It follows only the fields of message types that
//! the schema knows, and the schema has no recursive message type, so the
//! depth of the walk is the depth of the schema.

use crate::record::UnprocessedReason;

/// One message type: its fields.
pub(super) struct Message {
    pub(super) fields: &'static [Field],
}

/// One field of a message type.
pub(super) struct Field {
    /// The protobuf field number.
    pub(super) number: u32,
    /// For a field of a message type: the index of that type in `MESSAGES`.
    pub(super) message: Option<usize>,
}

include!(concat!(env!("OUT_DIR"), "/schema_fields.rs"));

/// Wire types of the protobuf encoding.
const VARINT: u64 = 0;
const FIXED64: u64 = 1;
const LENGTH_DELIMITED: u64 = 2;
const FIXED32: u64 = 5;

/// Returns `true` when `bytes`, a message of type `message`, holds a field
/// that the vendored schema does not know, at any depth.
///
/// # Errors
///
/// Returns [`UnprocessedReason::DecodeFailed`] when the bytes are not valid
/// protobuf. That does not occur after a successful decode with `prost`.
pub(super) fn has_unknown_fields(
    mut bytes: &[u8],
    message: usize,
) -> Result<bool, UnprocessedReason> {
    let fields = MESSAGES
        .get(message)
        .ok_or(UnprocessedReason::DecodeFailed)?
        .fields;
    while !bytes.is_empty() {
        let key = varint(&mut bytes)?;
        let number = key >> 3;
        let wire_type = key & 0b111;
        let Some(field) = fields
            .iter()
            .find(|field| u64::from(field.number) == number)
        else {
            return Ok(true);
        };
        match wire_type {
            VARINT => {
                varint(&mut bytes)?;
            }
            FIXED64 => bytes = advance(bytes, 8)?,
            FIXED32 => bytes = advance(bytes, 4)?,
            LENGTH_DELIMITED => {
                let length = usize::try_from(varint(&mut bytes)?)
                    .map_err(|_| UnprocessedReason::DecodeFailed)?;
                let content = bytes.get(..length).ok_or(UnprocessedReason::DecodeFailed)?;
                if let Some(child) = field.message
                    && has_unknown_fields(content, child)?
                {
                    return Ok(true);
                }
                bytes = advance(bytes, length)?;
            }
            // A group, or a wire type that protobuf does not define. No
            // field of the schema is a group, and `prost` rejects a known
            // field with a wrong wire type.
            _ => return Err(UnprocessedReason::DecodeFailed),
        }
    }
    Ok(false)
}

fn advance(bytes: &[u8], count: usize) -> Result<&[u8], UnprocessedReason> {
    bytes.get(count..).ok_or(UnprocessedReason::DecodeFailed)
}

/// Reads one base-128 varint of at most 10 bytes.
fn varint(bytes: &mut &[u8]) -> Result<u64, UnprocessedReason> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().take(10).enumerate() {
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            *bytes = bytes
                .get(index + 1..)
                .ok_or(UnprocessedReason::DecodeFailed)?;
            return Ok(value);
        }
    }
    Err(UnprocessedReason::DecodeFailed)
}
