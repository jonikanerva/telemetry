// The global session-loss handler. Any query or mutation that fails with
// the problem code `unauthenticated` means that the server no longer
// accepts the session (expiry, logout in another tab, server restart).
// The handler removes all cached data and opens the login page with the
// current page as the target after the login.
//
// `invalid_credentials` (a wrong password) is not a session loss and
// does nothing here. The session query is skipped: the protected route
// handles its 401 itself (auth/protect.ts).
import type { QueryClient } from '@tanstack/react-query';

import type { CacheErrorHandler } from '../query-client.js';
import { LOGIN_PATH, safeRedirect } from './safe-redirect.js';
import { SESSION_PROBE, isSessionLoss } from './session.js';

export type SessionLossDeps = {
  readonly queryClient: QueryClient;
  /** The page origin, for example `window.location.origin`. */
  readonly origin: string;
  /** The internal href (path, query, hash) of the current page. */
  readonly currentHref: () => string;
  /** Opens the login page with a safe target after the login. */
  readonly navigateToLogin: (redirect: string) => void;
};

export function createSessionLossHandler(deps: SessionLossDeps): CacheErrorHandler {
  return (error, meta) => {
    if (!isSessionLoss(error) || meta?.[SESSION_PROBE] === true) {
      return;
    }
    const href = deps.currentHref();
    deps.queryClient.clear();
    if (isOnLoginPage(href, deps.origin)) {
      return;
    }
    deps.navigateToLogin(safeRedirect(href, deps.origin));
  };
}

function isOnLoginPage(href: string, origin: string): boolean {
  try {
    return new URL(href, origin).pathname === LOGIN_PATH;
  } catch {
    return false;
  }
}
