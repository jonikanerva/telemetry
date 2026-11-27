# ADR 0012: Web sessions and login

- Status: Proposed, 28 November 2026. Lead decision under the owner
  decision of ADR 0003. It becomes Accepted when the independent security
  reviewer of the PR approves it.
- Scope: the session system, the login, the logout, the CSRF rule, and
  the login throttle of the web service `telemetry-web`. ADR 0003 holds
  the owner decision (one password, an Argon2id hash, a session cookie,
  a global throttle). This ADR holds the technical decisions and the
  threat model that `docs/stack-web-profile.md` §2.5 requires for a
  first-party session system.

## Context

- ADR 0003 leaves the place of the session data to the first web PR.
- The owner gave these values for the task: at most 8 sessions, an
  absolute lifetime of 30 days, an idle lifetime of 7 days, the cookie
  `__Host-telemetry_session` with `Secure`, `HttpOnly`,
  `SameSite=Strict`, and `Path=/`, the public origin
  `https://tesla.calmdonut.com`, and session data in the memory of the
  server. A restart or a deploy ends all sessions.
- The web service runs one process on Railway. A Railway deploy with an
  overlap of 0 s stops the old process before the new one serves.
- The data shows where the owner is and when the owner is away
  (`VISION.md`). Neon has 100 CU-hours each month, so a request without
  a session must not wake the database.
- The login page is reachable from the internet (ADR 0003).

## Decision

1. **Session store.** A first-party store in the memory of the process
   (`crates/server/src/auth/store.rs`). It holds at most 8 sessions. A new
   session replaces the session with the oldest login time when the store
   is full. Ended sessions leave first.
2. **Token.** 32 bytes from the random source of the operating system,
   64 lowercase hex digits in the cookie. The store compares a token with
   every stored token in constant time. A login always makes a new token.
   The server never accepts a token that the client chose.
3. **Lifetimes.** The server enforces both lifetimes on its monotonic
   clock. Absolute: 30 days after the login. Idle: 7 days after the last
   request with the session. Each request with a valid session moves the
   idle deadline; the absolute deadline never moves. At the deadline
   itself the session has ended.
4. **Cookie.** `__Host-telemetry_session=<token>; Max-Age=<remaining
   absolute seconds>; Path=/; Secure; HttpOnly; SameSite=Strict`. Logout
   and a 401 response to a request with a dead cookie send the same name
   with `Max-Age=0`.
5. **Fixation and rotation.** A login ends every session that the request
   presents, then makes a new session. There is no other rotation: a
   session has one token for its whole life, and the lifetimes bound it.
6. **Revocation and logout.** `DELETE /api/v1/session` ends the session of
   the request at once; a copy of the cookie stops working. A restart
   ends every session; this is also the "log out everywhere" mechanism.
   A password change needs a new variable value and a redeploy (ADR
   0003), so it also ends every session.
7. **Concurrent sessions.** Up to 8 sessions at the same time, for
   example a desktop and a phone. Logout ends only its own session.
8. **CSRF rule.** Before the handler of every API request with a method
   other than `GET` and `HEAD`: one `Origin` header equal to
   `WEB_PUBLIC_ORIGIN`, or without `Origin` the header
   `Sec-Fetch-Site: same-origin`; else 403 `forbidden_origin`. The login
   also requires `Content-Type: application/json` (415), so a plain HTML
   form cannot send it. `SameSite=Strict` is the second layer. `GET`
   handlers change nothing except the idle deadline.
9. **Guard.** One middleware runs before every documented API route. A
   route outside the public list (`/health`, `/api/v1/session`) needs a
   valid session, else 401 `unauthenticated`. The rule is "deny unless
   listed", so a new route of a later slice is protected without a
   change of the guard. No handler, and so no database query, runs for a
   request without a session.
10. **Throttle.** Global, not for each IP address (ADR 0003):
    - a token bucket of 5 attempts that gets one attempt back each 12 s
      (at most 300 attempts each hour);
    - one verification slot: at most one Argon2 verification runs at a
      time; a second attempt gets 429 at once;
    - 429 `login_throttled` with `Retry-After` and
      `retry_after_seconds`: the time to the next attempt, rounded up,
      or 1 s for a busy slot;
    - a malformed request (wrong content type, body above 4 KiB, not a
      JSON object with only `password`, password outside 1 to 1024
      bytes) gets its problem and costs no attempt.
11. **Password check.** Argon2id from the crate `argon2` 0.6.0. The
    verification runs on a blocking thread and owns the slot until it
    ends, also when the client leaves. The start of the server rejects a
    hash outside these bounds, without an echo of the value: algorithm
    `argon2id`, version 19, memory 19456 to 131072 KiB, 2 to 10 passes,
    1 to 4 lanes, a salt of 16 to 64 bytes, a 32-byte output, no key id,
    and no associated data. The command `telemetry-web-server
    hash-password` makes the hash with m=65536, t=3, p=1 from a password
    on standard input (16 to 1024 bytes, one optional line end).
12. **Logs.** Events with a static name and the field `reason` from a
    closed set only. No password, hash, cookie, token, or IP address.

## Threat model

Assets: the session token, the password, the hash, and the location
history behind the API.

| Threat | Control | Residual risk |
| ------ | ------- | ------------- |
| Online password guessing | Global bucket (300 attempts each hour at most); a long random password from a password manager (ADR 0003) | None in practice for a random password of 20 or more characters. |
| Offline guessing with a leaked hash | Argon2id at 64 MiB and 3 passes | A holder of the Railway variables can guess offline; a long random password keeps this impractical (accepted, ADR 0003). |
| Owner lockout by an attacker who sends attempts | The bucket is global and gets one attempt back each 12 s; the 429 response gives the wait | During an attack, the owner competes with the attacker for one attempt each 12 s and can fail to log in while the attack lasts (accepted, ADR 0003). |
| CPU and memory exhaustion by Argon2 | One verification at a time; body limit 4 KiB; a bounded hash cost at the start | One verification uses 64 MiB and about 0.2 s of one core (measured on an Apple M3; the Railway CPU is not measured). |
| Neon CU-hour exhaustion by unauthenticated requests | The guard runs before every handler; `/health` needs no database | None. |
| Cross-site request forgery | Origin rule, JSON content type, `SameSite=Strict` | None known. |
| Session fixation | `__Host-` prefix; a login always makes a new token and ends the presented session | None known. |
| Cookie theft by script (XSS) | `HttpOnly`; the CSP `script-src 'self'` of ADR 0010; no inline script | A script injection in the SPA could still send requests while the page is open. |
| Cookie theft on the network | HTTPS only through the Cloudflare DNS record and the Railway certificate; HSTS; `Secure` | None known. |
| A stolen cookie after logout or expiry | Server-side revocation and lifetimes; the server holds the only valid copy | A stolen cookie works until logout, expiry, or a restart. |
| Token guessing | 256 random bits; constant-time compare | None in practice. |
| Information leaks | Static problem texts; the log allowlist; redacted `Debug` of the hash, the token, and the password | None known. |
| Second factor | None (ADR 0003) | Accepted by the owner. |

## Key alternatives

- **Signed or encrypted cookie without server state.** Survives a deploy
  and needs no store. A logout only drops the cookie: a stolen copy stays
  valid until it expires or the key changes. It fails the revocation rule
  of §2.5. Rejected.
- **A PostgreSQL table of sessions.** Survives a deploy and allows real
  revocation. Every session check would wake Neon, also `GET
  /api/v1/session` on each page load, and the web role would need write
  privileges and a migration. Rejected for now; reassess below.
- **`tower-sessions` 0.15.0.** Its memory store has no bound, it uses its
  own clock (not testable with the paused Tokio clock), and it adds the
  `time` crate as a second time library. Rejected.
- **A throttle for each IP address.** Behind the Railway proxy an address
  is not a reliable identity (ADR 0003). Rejected.

## Consequences

- Every web deploy and every restart logs the owner out.
- The store needs no database, so a login and a session check never wake
  Neon.
- The rules are first-party security code. Tests cover each rule
  (`crates/server/src/auth/**`, `e2e/login.spec.ts`); the PR maps each
  rule to its code and test.
- The browser tests run one worker, because the store and the throttle
  are global in the one test server.

## Cost of reversal

Low. The store is one module behind the guard and the three handlers. A
change to a PostgreSQL store or to Cloudflare Access in front of the
service changes no stored data and no client code.

## Reassessment

- The owner finds the logout at each deploy a problem: reassess the
  PostgreSQL store.
- A security finding in the login path.
- The owner wants a second factor (ADR 0003).
- Railway changes the deploy overlap so that two processes serve at the
  same time: sessions would then depend on the process that answers.
