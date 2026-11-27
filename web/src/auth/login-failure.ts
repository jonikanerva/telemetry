// The failure of a login request, as the login form shows it.
//
// - `invalid_credentials`: a wrong password. The form clears the field.
// - `login_throttled`: too many attempts. The form keeps the input and
//   shows the wait time when the server sends it.
// - Every other failure (network, timeout, invalid or unknown body, 400,
//   403, 413, 415, 5xx, a programmer defect): the service did not answer
//   as expected. The form keeps the input.
import { ApiRequestError } from 'telemetry-web-api-client';

import { fi } from '../i18n/fi.js';

export type LoginFailure =
  | { readonly kind: 'invalid_credentials' }
  | { readonly kind: 'throttled'; readonly retryAfterSeconds: number | null }
  | { readonly kind: 'unavailable' };

export function classifyLoginFailure(error: unknown): LoginFailure {
  if (!(error instanceof ApiRequestError) || error.error.kind !== 'problem') {
    return { kind: 'unavailable' };
  }
  const problem = error.error.problem;
  if (problem.code === 'invalid_credentials') {
    return { kind: 'invalid_credentials' };
  }
  if (problem.code === 'login_throttled') {
    return { kind: 'throttled', retryAfterSeconds: problem.retry_after_seconds ?? null };
  }
  return { kind: 'unavailable' };
}

export function loginFailureMessage(failure: LoginFailure): string {
  switch (failure.kind) {
    case 'invalid_credentials':
      return fi.loginInvalidCredentials;
    case 'throttled':
      return failure.retryAfterSeconds === null
        ? fi.loginThrottledNoTime
        : fi.loginThrottled(failure.retryAfterSeconds);
    case 'unavailable':
      return fi.loginUnavailable;
    default:
      return unknownFailure(failure);
  }
}

/** Whether the form clears the password field after the failure. */
export function clearsPassword(failure: LoginFailure): boolean {
  return failure.kind === 'invalid_credentials';
}

function unknownFailure(_failure: never): string {
  return fi.loginUnavailable;
}
