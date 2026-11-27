import { QueryClient } from '@tanstack/react-query';
import { ApiRequestError } from 'telemetry-web-api-client';
import { describe, expect, it, vi } from 'vitest';

import { createQueryClient } from '../query-client.js';
import { SESSION_PROBE, SESSION_QUERY_KEY } from './session.js';
import { createSessionLossHandler } from './session-loss.js';
import { ORIGIN, SESSION, problemError } from './test-support.js';

function setup(currentHref = '/battery?range=7d#chart') {
  const navigateToLogin = vi.fn<(redirect: string) => void>();
  let handler: ReturnType<typeof createSessionLossHandler> = () => undefined;
  const queryClient = createQueryClient({
    onError: (error, meta) => {
      handler(error, meta);
    },
  });
  handler = createSessionLossHandler({ queryClient, origin: ORIGIN, currentHref: () => currentHref, navigateToLogin });
  queryClient.setQueryData(SESSION_QUERY_KEY, SESSION);
  queryClient.setQueryData(['battery'], { level: 80 });
  return { queryClient, navigateToLogin };
}

async function failQuery(queryClient: QueryClient, error: unknown, meta?: Record<string, unknown>) {
  await expect(
    queryClient.query({
      queryKey: ['other'],
      queryFn: () => Promise.reject(error instanceof Error ? error : new Error('x')),
      retry: false,
      ...(meta === undefined ? {} : { meta }),
    }),
  ).rejects.toBe(error);
}

async function failMutation(queryClient: QueryClient, error: Error) {
  await expect(
    queryClient
      .getMutationCache()
      .build(queryClient, { mutationFn: () => Promise.reject(error) })
      .execute(undefined),
  ).rejects.toBe(error);
}

describe('the session-loss handler', () => {
  it('clears the cache and opens the login page after a 401 of a query', async () => {
    const { queryClient, navigateToLogin } = setup();
    await failQuery(queryClient, problemError('unauthenticated', 401));
    expect(queryClient.getQueryData(SESSION_QUERY_KEY)).toBeUndefined();
    expect(queryClient.getQueryData(['battery'])).toBeUndefined();
    expect(navigateToLogin).toHaveBeenCalledExactlyOnceWith('/battery?range=7d#chart');
  });

  it('clears the cache and opens the login page after a 401 of a mutation', async () => {
    const { queryClient, navigateToLogin } = setup('/');
    await failMutation(queryClient, problemError('unauthenticated', 401));
    expect(queryClient.getQueryData(['battery'])).toBeUndefined();
    expect(navigateToLogin).toHaveBeenCalledExactlyOnceWith('/');
  });

  it.each([
    ['invalid_credentials', problemError('invalid_credentials', 401)],
    ['login_throttled', problemError('login_throttled', 429, 30)],
    ['forbidden_origin', problemError('forbidden_origin', 403)],
    ['service_busy', problemError('service_busy', 503)],
    ['a network failure', new ApiRequestError({ kind: 'network' })],
    ['an invalid 401 body', new ApiRequestError({ kind: 'invalid_response', status: 401 })],
    ['a programmer defect', new Error('defect')],
  ])('does nothing for %s', async (_name, error) => {
    const { queryClient, navigateToLogin } = setup();
    await failMutation(queryClient, error);
    await failQuery(queryClient, error);
    expect(queryClient.getQueryData(SESSION_QUERY_KEY)).toEqual(SESSION);
    expect(queryClient.getQueryData(['battery'])).toEqual({ level: 80 });
    expect(navigateToLogin).not.toHaveBeenCalled();
  });

  it('skips the session query, which the protected route handles', async () => {
    const { queryClient, navigateToLogin } = setup();
    await failQuery(queryClient, problemError('unauthenticated', 401), { [SESSION_PROBE]: true });
    expect(queryClient.getQueryData(['battery'])).toEqual({ level: 80 });
    expect(navigateToLogin).not.toHaveBeenCalled();
  });

  it('clears the cache but does not navigate on the login page', async () => {
    const { queryClient, navigateToLogin } = setup('/login?redirect=%2F');
    await failQuery(queryClient, problemError('unauthenticated', 401));
    expect(queryClient.getQueryData(['battery'])).toBeUndefined();
    expect(navigateToLogin).not.toHaveBeenCalled();
  });

  it('passes only a safe redirect target', async () => {
    const { queryClient, navigateToLogin } = setup('//evil.example/x');
    await failQuery(queryClient, problemError('unauthenticated', 401));
    expect(navigateToLogin).toHaveBeenCalledExactlyOnceWith('/');
  });
});
