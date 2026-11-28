import { ApiRequestError } from 'telemetry-web-api-client';
import { describe, expect, it } from 'vitest';

import { isConflictingCookies } from './conflicting-cookies.js';
import { problemError } from './test-support.js';

describe('the two-cookie state', () => {
  it('is a 400 invalid_request problem', () => {
    expect(isConflictingCookies(problemError('invalid_request', 400))).toBe(true);
  });

  it.each([
    ['a 401', problemError('unauthenticated', 401)],
    ['a 403', problemError('forbidden_origin', 403)],
    ['a 503', problemError('database_unavailable', 503)],
    ['a 400 without a valid problem body', new ApiRequestError({ kind: 'invalid_response', status: 400 })],
    ['a network failure', new ApiRequestError({ kind: 'network' })],
    ['another error', new Error('boom')],
    ['no error', undefined],
  ])('is not %s', (_name, error) => {
    expect(isConflictingCookies(error)).toBe(false);
  });
});
