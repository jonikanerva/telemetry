// The logout flow. DELETE /api/v1/session ends the session on the
// server. Only a confirmed revocation (status 204, an ok facade result)
// counts as a logout: then the browser drops all cached data and opens
// the login page.
//
// Any other outcome (a network failure, a timeout, an invalid response,
// a problem such as 403 or 5xx, or a thrown error) leaves the cookie and
// the server session possibly valid. The browser still drops all cached
// data at once, so the UI shows no protected data, and it opens the
// failure page `/logout` (routes/logout.tsx). It never opens the login
// page: the owner must not believe that the logout worked. Data shown
// later is fetched again with the session, which may still be valid.
//
// The failure page offers a retry. One click sends one request; there is
// no automatic retry.
import type { QueryClient } from '@tanstack/react-query';
import type { ApiResult } from 'telemetry-web-api-client';

import { fi } from '../i18n/fi.js';
import { LOGIN_PATH } from './safe-redirect.js';

export const LOGOUT_PATH = '/logout';

export type LogoutOutcome = 'confirmed' | 'unconfirmed';

export type LogoutTarget = {
  readonly to: typeof LOGIN_PATH | typeof LOGOUT_PATH;
  /** True replaces the current history entry; false pushes a new one. */
  readonly replace: boolean;
};

export type LogoutDeps = {
  readonly logout: () => Promise<ApiResult<undefined>>;
  readonly queryClient: QueryClient;
  readonly navigate: (target: LogoutTarget) => Promise<void>;
};

/** Sends the logout request once. Only an ok result is a confirmation. */
export async function revokeSession(logout: LogoutDeps['logout']): Promise<LogoutOutcome> {
  try {
    const result = await logout();
    return result.ok ? 'confirmed' : 'unconfirmed';
  } catch {
    // The facade returns failures as values; a thrown error is still no
    // confirmation.
    return 'unconfirmed';
  }
}

/** The logout of the header button. Both targets are new history entries. */
export async function performLogout(deps: LogoutDeps): Promise<LogoutOutcome> {
  const outcome = await revokeSession(deps.logout);
  // Clear first: the login page sends an existing session back to the
  // target page, and the failure page must show no cached data.
  deps.queryClient.clear();
  await deps.navigate({ to: outcome === 'confirmed' ? LOGIN_PATH : LOGOUT_PATH, replace: false });
  return outcome;
}

/** The guard of the retry button: true while a request runs. */
export type LogoutAttempt = { running: boolean };

export type LogoutRetryDeps = LogoutDeps & {
  readonly setPending: (pending: boolean) => void;
  /** The text of the alert region; null empties the region. */
  readonly setMessage: (message: string | null) => void;
};

/**
 * One retry from the failure page. A click while a request runs is
 * ignored. The alert text is removed at the start and set again after a
 * failure, so a screen reader announces the failure again.
 */
export async function retryLogout(attempt: LogoutAttempt, deps: LogoutRetryDeps): Promise<LogoutOutcome | 'ignored'> {
  if (attempt.running) {
    return 'ignored';
  }
  attempt.running = true;
  deps.setPending(true);
  deps.setMessage(null);
  try {
    const outcome = await revokeSession(deps.logout);
    if (outcome === 'confirmed') {
      deps.queryClient.clear();
      // Replace: Back must not return to the failure page.
      await deps.navigate({ to: LOGIN_PATH, replace: true });
    } else {
      deps.setMessage(fi.logoutFailedBody);
    }
    return outcome;
  } finally {
    attempt.running = false;
    deps.setPending(false);
  }
}
