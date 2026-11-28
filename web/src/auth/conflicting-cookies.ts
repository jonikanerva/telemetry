// The two-cookie state (ADR 0012, threat-model row of a second session
// cookie pair). A request with more than one session cookie gets 400
// `invalid_request` on every route, also on the session routes. The
// browser then cannot log in or read its session until the owner deletes
// the cookies of the site. The UI reads this meaning only from the
// session read and the login. Known limit: the login also gets 400 when
// the server cannot read its body or the password is longer than 1024
// UTF-8 bytes; the field limits 1024 UTF-16 code units, so a long
// password with non-ASCII characters can pass the field. The text of
// this state then names the wrong cause. The owner uses a generated
// ASCII password, so the case is improbable.
import { ApiRequestError } from 'telemetry-web-api-client';

/** True when a failed session read or login is the two-cookie state. */
export function isConflictingCookies(error: unknown): boolean {
  return (
    error instanceof ApiRequestError &&
    error.error.kind === 'problem' &&
    error.error.problem.code === 'invalid_request' &&
    error.error.problem.status === 400
  );
}
