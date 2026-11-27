// The session of the browser: the query of GET /api/v1/session and the
// rule that tells a session loss from other failures.
//
// The session cookie is HttpOnly, so the page knows about a session only
// from the session body. The query cache holds that body under
// SESSION_QUERY_KEY. A login writes it, and a logout or a session loss
// clears the whole cache.
import { queryOptions } from '@tanstack/react-query';
import { ApiRequestError, api, isUnauthenticated, unwrap } from 'telemetry-web-api-client';
import type { ApiClient, Session } from 'telemetry-web-api-client';

import { shouldRetry } from '../query-client.js';

export const SESSION_QUERY_KEY = ['session'] as const;

/**
 * The `meta` key of the session query. The protected route handles the
 * session loss of this query itself (auth/protect.ts), so the global
 * session-loss handler skips it. Two navigations to the login page would
 * otherwise race.
 */
export const SESSION_PROBE = 'sessionProbe';

export function sessionQueryOptions(client: Pick<ApiClient, 'getSession'>) {
  return queryOptions({
    queryKey: SESSION_QUERY_KEY,
    queryFn: async ({ signal }): Promise<Session> => unwrap(await client.getSession({ signal })),
    // The session body changes only through a login, a logout, or a
    // session loss. Each of them writes or clears the cache.
    staleTime: Infinity,
    gcTime: Infinity,
    // A 401 is never retried (shouldRetry); a busy server is retried once.
    retry: shouldRetry,
    meta: { [SESSION_PROBE]: true },
  });
}

export const sessionQuery = sessionQueryOptions(api);

/** Whether the error of a query or mutation means that the session ended. */
export function isSessionLoss(error: unknown): boolean {
  return error instanceof ApiRequestError && isUnauthenticated(error.error);
}
