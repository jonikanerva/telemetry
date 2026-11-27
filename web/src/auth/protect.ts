// The session check of the protected layout route. It runs in
// `beforeLoad`, before any protected page renders.
//
// - A cached session body: the page opens without a request.
// - No cached body: GET /api/v1/session.
// - 401 `unauthenticated`: clear the cache and redirect to the login page
//   with the requested page as the target.
// - Any other failure: the error reaches the error component of the
//   route, which shows the unavailable state. A failure never opens the
//   login page and never shows an empty page.
import type { QueryClient } from '@tanstack/react-query';
import { redirect } from '@tanstack/react-router';
import type { Session } from 'telemetry-web-api-client';

import { LOGIN_PATH, safeRedirect } from './safe-redirect.js';
import { isSessionLoss } from './session.js';
import type { sessionQueryOptions } from './session.js';

export async function requireSession(
  queryClient: QueryClient,
  query: ReturnType<typeof sessionQueryOptions>,
  href: string,
  origin: string,
): Promise<Session> {
  try {
    // 'static': a cached body is returned without a request; without a
    // cached body the query fetches.
    return await queryClient.query({ ...query, staleTime: 'static' });
  } catch (error: unknown) {
    if (isSessionLoss(error)) {
      queryClient.clear();
      // `throw: true` makes TanStack Router throw the redirect itself.
      redirect({ to: LOGIN_PATH, search: { redirect: safeRedirect(href, origin) }, throw: true });
    }
    throw error;
  }
}
