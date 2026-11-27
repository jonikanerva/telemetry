import type { ApiResult } from 'telemetry-web-api-client';
import { describe, expect, it, vi } from 'vitest';

import { fi } from '../i18n/fi.js';
import { createQueryClient } from '../query-client.js';
import { performLogout, retryLogout } from './logout.js';
import type { LogoutAttempt, LogoutTarget } from './logout.js';
import { SESSION_QUERY_KEY } from './session.js';
import { SESSION, problemError } from './test-support.js';

type Logout = () => Promise<ApiResult<undefined>>;

const SUCCESS: ApiResult<undefined> = { ok: true, data: undefined };

const FAILURES: readonly (readonly [string, ApiResult<undefined>])[] = [
  ['a network failure', { ok: false, error: { kind: 'network' } }],
  ['a timeout', { ok: false, error: { kind: 'timeout' } }],
  ['an invalid response', { ok: false, error: { kind: 'invalid_response', status: 200 } }],
  ['a 403 problem', { ok: false, error: problemError('forbidden_origin', 403).error }],
  ['a 503 problem', { ok: false, error: problemError('service_busy', 503).error }],
];

function setup(logout: Logout) {
  const queryClient = createQueryClient();
  queryClient.setQueryData(SESSION_QUERY_KEY, SESSION);
  queryClient.setQueryData(['battery'], { level: 80 });
  const order: string[] = [];
  const targets: LogoutTarget[] = [];
  const navigate = vi.fn((target: LogoutTarget) => {
    order.push(queryClient.getQueryData(['battery']) === undefined ? 'navigate after clear' : 'navigate');
    targets.push(target);
    return Promise.resolve();
  });
  return { queryClient, navigate, logout: vi.fn(logout), order, targets };
}

describe('performLogout', () => {
  it('clears the cache and opens the login page as a new entry after a 204', async () => {
    const { queryClient, navigate, logout, order, targets } = setup(() => Promise.resolve(SUCCESS));
    await expect(performLogout({ logout, queryClient, navigate })).resolves.toBe('confirmed');
    expect(logout).toHaveBeenCalledOnce();
    expect(queryClient.getQueryData(SESSION_QUERY_KEY)).toBeUndefined();
    expect(order).toEqual(['navigate after clear']);
    expect(targets).toEqual([{ to: '/login', replace: false }]);
  });

  it.each(FAILURES)(
    'clears the cache and opens the failure page, not the login page, after %s',
    async (_name, result) => {
      const { queryClient, navigate, logout, order, targets } = setup(() => Promise.resolve(result));
      await expect(performLogout({ logout, queryClient, navigate })).resolves.toBe('unconfirmed');
      expect(logout).toHaveBeenCalledOnce();
      expect(queryClient.getQueryData(SESSION_QUERY_KEY)).toBeUndefined();
      expect(queryClient.getQueryData(['battery'])).toBeUndefined();
      expect(order).toEqual(['navigate after clear']);
      expect(targets).toEqual([{ to: '/logout', replace: false }]);
    },
  );

  it('treats a thrown error of the facade as a failure', async () => {
    const { queryClient, navigate, logout, targets } = setup(async () => Promise.reject(new Error('aborted')));
    await expect(performLogout({ logout, queryClient, navigate })).resolves.toBe('unconfirmed');
    expect(queryClient.getQueryData(['battery'])).toBeUndefined();
    expect(targets).toEqual([{ to: '/logout', replace: false }]);
  });
});

function retrySetup(logout: Logout) {
  const base = setup(logout);
  const messages: (string | null)[] = [];
  const pending: boolean[] = [];
  const attempt: LogoutAttempt = { running: false };
  const deps = {
    logout: base.logout,
    queryClient: base.queryClient,
    navigate: base.navigate,
    setPending: (value: boolean) => {
      pending.push(value);
    },
    setMessage: (value: string | null) => {
      messages.push(value);
    },
  };
  return { ...base, deps, attempt, messages, pending };
}

describe('retryLogout', () => {
  it('opens the login page with replace after a 204', async () => {
    const { deps, attempt, logout, targets, order, pending } = retrySetup(() => Promise.resolve(SUCCESS));
    await expect(retryLogout(attempt, deps)).resolves.toBe('confirmed');
    expect(logout).toHaveBeenCalledOnce();
    expect(order).toEqual(['navigate after clear']);
    expect(targets).toEqual([{ to: '/login', replace: true }]);
    expect(pending).toEqual([true, false]);
    expect(attempt.running).toBe(false);
  });

  it.each(FAILURES)('stays and sets the message again after %s', async (_name, result) => {
    const { deps, attempt, logout, navigate, messages, pending } = retrySetup(() => Promise.resolve(result));
    await expect(retryLogout(attempt, deps)).resolves.toBe('unconfirmed');
    expect(logout).toHaveBeenCalledOnce();
    expect(navigate).not.toHaveBeenCalled();
    // Removed first and set again, so a screen reader announces it again.
    expect(messages).toEqual([null, fi.logoutFailedBody]);
    expect(pending).toEqual([true, false]);
    expect(attempt.running).toBe(false);
  });

  it('stays after a thrown error of the facade', async () => {
    const { deps, attempt, navigate, messages } = retrySetup(async () => Promise.reject(new Error('aborted')));
    await expect(retryLogout(attempt, deps)).resolves.toBe('unconfirmed');
    expect(navigate).not.toHaveBeenCalled();
    expect(messages).toEqual([null, fi.logoutFailedBody]);
  });

  it('sends one request for each click and ignores a click while a request runs', async () => {
    let resolve: (result: ApiResult<undefined>) => void = () => undefined;
    const { deps, attempt, logout } = retrySetup(
      async () =>
        new Promise<ApiResult<undefined>>((done) => {
          resolve = done;
        }),
    );
    const first = retryLogout(attempt, deps);
    expect(attempt.running).toBe(true);
    await expect(retryLogout(attempt, deps)).resolves.toBe('ignored');
    expect(logout).toHaveBeenCalledOnce();
    resolve({ ok: false, error: { kind: 'network' } });
    await expect(first).resolves.toBe('unconfirmed');

    const second = retryLogout(attempt, deps);
    resolve({ ok: false, error: { kind: 'network' } });
    await expect(second).resolves.toBe('unconfirmed');
    expect(logout).toHaveBeenCalledTimes(2);
  });
});
