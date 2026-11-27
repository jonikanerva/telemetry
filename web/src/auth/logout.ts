// The logout flow. DELETE /api/v1/session ends the session on the
// server. The browser drops all cached data and opens the login page
// after success and after failure alike: after a failure the session
// still expires on the server, and the browser must not keep data.
import type { QueryClient } from '@tanstack/react-query';
import type { ApiResult } from 'telemetry-web-api-client';

export type LogoutDeps = {
  readonly logout: () => Promise<ApiResult<undefined>>;
  readonly queryClient: QueryClient;
  /** Opens the login page as a new history entry (push, not replace). */
  readonly navigateToLogin: () => Promise<void>;
};

export async function performLogout(deps: LogoutDeps): Promise<void> {
  try {
    await deps.logout();
  } finally {
    // Clear first: the login page sends an existing session back to the
    // target page.
    deps.queryClient.clear();
    await deps.navigateToLogin();
  }
}
