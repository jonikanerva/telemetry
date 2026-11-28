import { ApiRequestError } from 'telemetry-web-api-client';
import { describe, expect, it } from 'vitest';

import { fi } from '../i18n/fi.js';
import { classifyLoginFailure, clearsPassword, loginFailureMessage } from './login-failure.js';
import { problemError } from './test-support.js';

const UNAVAILABLE = 'Kirjautuminen ei onnistunut. Palvelu ei vastannut. Yritä hetken kuluttua uudelleen.';

describe('the login failure', () => {
  it('explains two session cookies on a 400 invalid_request and keeps the input', () => {
    const failure = classifyLoginFailure(problemError('invalid_request', 400));
    expect(failure).toEqual({ kind: 'conflicting_cookies' });
    expect(loginFailureMessage(failure)).toBe(fi.conflictingCookies);
    expect(loginFailureMessage(failure)).toContain('Poista tämän sivuston evästeet');
    expect(clearsPassword(failure)).toBe(false);
  });

  it('shows a wrong password and clears the field', () => {
    const failure = classifyLoginFailure(problemError('invalid_credentials', 401));
    expect(loginFailureMessage(failure)).toBe('Salasana on väärä.');
    expect(clearsPassword(failure)).toBe(true);
  });

  it('shows the wait time of a throttled login and keeps the input', () => {
    const failure = classifyLoginFailure(problemError('login_throttled', 429, 42));
    expect(loginFailureMessage(failure)).toBe('Liian monta kirjautumisyritystä. Yritä uudelleen 42 sekunnin kuluttua.');
    expect(clearsPassword(failure)).toBe(false);
  });

  it('shows a throttled login without a wait time', () => {
    const failure = classifyLoginFailure(problemError('login_throttled', 429));
    expect(loginFailureMessage(failure)).toBe('Liian monta kirjautumisyritystä. Yritä hetken kuluttua uudelleen.');
    expect(clearsPassword(failure)).toBe(false);
  });

  it.each([
    ['a network failure', new ApiRequestError({ kind: 'network' })],
    ['a timeout', new ApiRequestError({ kind: 'timeout' })],
    ['an invalid response', new ApiRequestError({ kind: 'invalid_response', status: 200 })],
    ['an unknown problem code (invalid_response)', new ApiRequestError({ kind: 'invalid_response', status: 401 })],
    ['a 502 page of a proxy', new ApiRequestError({ kind: 'invalid_response', status: 502 })],
    ['a 500', problemError('internal_error', 500)],
    ['a 503', problemError('database_unavailable', 503)],
    ['a 403', problemError('forbidden_origin', 403)],
    ['a 413', problemError('payload_too_large', 413)],
    ['a 415', problemError('unsupported_media_type', 415)],
    ['unauthenticated', problemError('unauthenticated', 401)],
    ['a programmer defect', new Error('defect')],
    ['a non-error value', 'text'],
  ])('shows %s as unavailable and keeps the input', (_name, error) => {
    const failure = classifyLoginFailure(error);
    expect(failure).toEqual({ kind: 'unavailable' });
    expect(loginFailureMessage(failure)).toBe(UNAVAILABLE);
    expect(clearsPassword(failure)).toBe(false);
  });
});
