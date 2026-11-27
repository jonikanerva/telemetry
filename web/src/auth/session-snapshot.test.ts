import { describe, expect, it, vi } from 'vitest';

import { createQueryClient } from '../query-client.js';
import { SESSION_QUERY_KEY } from './session.js';
import { sessionStore } from './session-snapshot.js';
import { SESSION } from './test-support.js';

describe('sessionStore', () => {
  it('follows a login and a clear, and never fetches', () => {
    const queryClient = createQueryClient();
    const store = sessionStore(queryClient);
    const listener = vi.fn();
    const unsubscribe = store.subscribe(listener);
    expect(store.getSnapshot()).toBeUndefined();

    queryClient.setQueryData(SESSION_QUERY_KEY, SESSION);
    expect(store.getSnapshot()).toEqual(SESSION);
    // A stable reference, as useSyncExternalStore requires.
    expect(store.getSnapshot()).toBe(store.getSnapshot());
    const callsAfterLogin = listener.mock.calls.length;
    expect(callsAfterLogin).toBeGreaterThan(0);

    queryClient.clear();
    expect(store.getSnapshot()).toBeUndefined();
    expect(listener.mock.calls.length).toBeGreaterThan(callsAfterLogin);
    expect(queryClient.isFetching()).toBe(0);
    unsubscribe();
  });
});
