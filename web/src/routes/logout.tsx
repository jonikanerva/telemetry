// The logout failure page `/logout`. It is public, not a child of the
// protected layout: the server did not confirm the logout, and the page
// must not ask for the session. `beforeLoad` empties the query cache, so
// the page never shows protected data or the header logout button, also
// after Back and Forward. The clear is local only and is not a logout.
import { createRoute } from '@tanstack/react-router';

import { LOGOUT_PATH } from '../auth/logout.js';
import { LogoutFailure } from '../auth/LogoutFailure.js';
import { rootRoute } from './root.js';

export const logoutRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: LOGOUT_PATH,
  beforeLoad: ({ context }) => {
    context.queryClient.clear();
  },
  component: LogoutFailure,
});
