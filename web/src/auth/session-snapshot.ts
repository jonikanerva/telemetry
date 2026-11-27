// A read-only view of the cached session body for components. It never
// sends a request: the header and the start page read the cache, and only
// the protected route fetches the session (auth/protect.ts).
//
// The store reads the cache on every cache event, so the snapshot becomes
// undefined at once after `queryClient.clear()`, also while no query
// observer exists.
import { useQueryClient } from '@tanstack/react-query';
import type { QueryClient } from '@tanstack/react-query';
import { useMemo, useSyncExternalStore } from 'react';
import type { Session } from 'telemetry-web-api-client';

import { SESSION_QUERY_KEY } from './session.js';

export type SessionStore = {
  readonly subscribe: (listener: () => void) => () => void;
  readonly getSnapshot: () => Session | undefined;
};

export function sessionStore(queryClient: QueryClient): SessionStore {
  return {
    subscribe: (listener) =>
      queryClient.getQueryCache().subscribe(() => {
        listener();
      }),
    getSnapshot: () => queryClient.getQueryData<Session>(SESSION_QUERY_KEY),
  };
}

/** The cached session body, or undefined without a session. */
export function useSession(): Session | undefined {
  const queryClient = useQueryClient();
  const store = useMemo(() => sessionStore(queryClient), [queryClient]);
  return useSyncExternalStore(store.subscribe, store.getSnapshot);
}
