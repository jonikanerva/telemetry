import type { ApiResult } from 'telemetry-web-api-client';
import { describe, expect, it, vi } from 'vitest';

import { createQueryClient } from '../query-client.js';
import { performLogout } from './logout.js';
import { SESSION_QUERY_KEY } from './session.js';
import { SESSION, problemError } from './test-support.js';

function setup(result: Promise<ApiResult<undefined>>) {
  const queryClient = createQueryClient();
  queryClient.setQueryData(SESSION_QUERY_KEY, SESSION);
  queryClient.setQueryData(['battery'], { level: 80 });
  const order: string[] = [];
  const navigateToLogin = vi.fn(() => {
    order.push(queryClient.getQueryData(SESSION_QUERY_KEY) === undefined ? 'navigate after clear' : 'navigate');
    return Promise.resolve();
  });
  const logout = vi.fn(() => result);
  return { queryClient, navigateToLogin, logout, order };
}

describe('performLogout', () => {
  it.each([
    ['success', Promise.resolve<ApiResult<undefined>>({ ok: true, data: undefined })],
    [
      'a 403 problem',
      Promise.resolve<ApiResult<undefined>>({ ok: false, error: problemError('forbidden_origin', 403).error }),
    ],
    ['a network failure', Promise.resolve<ApiResult<undefined>>({ ok: false, error: { kind: 'network' } })],
    ['a timeout', Promise.resolve<ApiResult<undefined>>({ ok: false, error: { kind: 'timeout' } })],
  ])('clears the cache and opens the login page after %s', async (_name, result) => {
    const { queryClient, navigateToLogin, logout, order } = setup(result);
    await performLogout({ logout, queryClient, navigateToLogin });
    expect(logout).toHaveBeenCalledOnce();
    expect(queryClient.getQueryData(SESSION_QUERY_KEY)).toBeUndefined();
    expect(queryClient.getQueryData(['battery'])).toBeUndefined();
    expect(order).toEqual(['navigate after clear']);
  });

  it('clears the cache and opens the login page when the call rejects', async () => {
    const { queryClient, navigateToLogin, logout, order } = setup(Promise.reject(new Error('aborted')));
    await expect(performLogout({ logout, queryClient, navigateToLogin })).rejects.toThrow('aborted');
    expect(queryClient.getQueryData(['battery'])).toBeUndefined();
    expect(order).toEqual(['navigate after clear']);
  });
});
