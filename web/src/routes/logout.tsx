// The logout failure page `/logout`. It is public, not a child of the
// protected layout: the server did not confirm the logout, and the page
// must not ask for the session. `beforeLoad` empties the query cache, so
// the page never shows protected data or the header logout button, also
// after Back and Forward. The clear is local only and is not a logout.
//
// Without a failed logout in this page session (a direct visit, a reload,
// a bookmark), the route goes to `/` instead: the protected layout then
// shows the application for a valid session and the login page otherwise.
import { createRoute, redirect } from '@tanstack/react-router';

import { LOGOUT_PATH, mayShowLogoutFailure } from '../auth/logout.js';
import { LogoutFailure } from '../auth/LogoutFailure.js';
import { rootRoute } from './root.js';

export const logoutRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: LOGOUT_PATH,
  beforeLoad: ({ context }) => {
    if (!mayShowLogoutFailure()) {
      // `throw: true` makes TanStack Router throw the redirect itself.
      redirect({ to: '/', replace: true, throw: true });
    }
    context.queryClient.clear();
  },
  component: LogoutFailure,
});
