import { isRedirect } from '@tanstack/react-router';
import { ApiRequestError } from 'telemetry-web-api-client';
import type { ApiResult, Session } from 'telemetry-web-api-client';
import { describe, expect, it, vi } from 'vitest';

import { createQueryClient } from '../query-client.js';
import { requireSession } from './protect.js';
import { SESSION_QUERY_KEY, sessionQueryOptions } from './session.js';
import { ORIGIN, SESSION, problemError } from './test-support.js';

function setup(result: ApiResult<Session>) {
  const getSession = vi.fn(() => Promise.resolve(result));
  const queryClient = createQueryClient();
  return { queryClient, getSession, query: sessionQueryOptions({ getSession }) };
}

describe('requireSession', () => {
  it('returns a cached session without a request', async () => {
    const { queryClient, getSession, query } = setup({ ok: false, error: { kind: 'network' } });
    queryClient.setQueryData(SESSION_QUERY_KEY, SESSION);
    await expect(requireSession(queryClient, query, '/', ORIGIN)).resolves.toEqual(SESSION);
    expect(getSession).not.toHaveBeenCalled();
  });

  it('fetches and caches the session without a cached body', async () => {
    const { queryClient, getSession, query } = setup({ ok: true, data: SESSION });
    await expect(requireSession(queryClient, query, '/', ORIGIN)).resolves.toEqual(SESSION);
    expect(getSession).toHaveBeenCalledOnce();
    expect(queryClient.getQueryData(SESSION_QUERY_KEY)).toEqual(SESSION);
  });

  it('redirects to the login page and clears the cache after a 401', async () => {
    const { queryClient, query } = setup({ ok: false, error: problemError('unauthenticated', 401).error });
    queryClient.setQueryData(['battery'], { level: 80 });
    const thrown: unknown = await requireSession(queryClient, query, '/battery?range=7d', ORIGIN).then(
      () => null,
      (error: unknown) => error,
    );
    expect(isRedirect(thrown)).toBe(true);
    if (isRedirect(thrown)) {
      expect(thrown.options.to).toBe('/login');
      expect(thrown.options.search).toEqual({ redirect: '/battery?range=7d' });
    }
    expect(queryClient.getQueryData(['battery'])).toBeUndefined();
  });

  // Other failures show the unavailable state, never the login page.
  it.each([
    ['a network failure', { kind: 'network' } as const],
    ['a timeout', { kind: 'timeout' } as const],
    ['an invalid response', { kind: 'invalid_response', status: 502 } as const],
    ['an internal error', problemError('internal_error', 500).error],
    ['invalid_credentials', problemError('invalid_credentials', 401).error],
  ])('rethrows %s without a redirect', async (_name, error) => {
    const { queryClient, query } = setup({ ok: false, error });
    queryClient.setQueryData(['battery'], { level: 80 });
    const thrown: unknown = await requireSession(queryClient, query, '/', ORIGIN).then(
      () => null,
      (reason: unknown) => reason,
    );
    expect(isRedirect(thrown)).toBe(false);
    expect(thrown).toBeInstanceOf(ApiRequestError);
    expect(queryClient.getQueryData(['battery'])).toEqual({ level: 80 });
  });

  it('fetches again after a failure', async () => {
    const getSession = vi
      .fn<() => Promise<ApiResult<Session>>>()
      .mockResolvedValueOnce({ ok: false, error: { kind: 'timeout' } })
      .mockResolvedValueOnce({ ok: true, data: SESSION });
    const queryClient = createQueryClient();
    const query = sessionQueryOptions({ getSession });
    await expect(requireSession(queryClient, query, '/', ORIGIN)).rejects.toBeInstanceOf(ApiRequestError);
    await expect(requireSession(queryClient, query, '/', ORIGIN)).resolves.toEqual(SESSION);
  });
});
