// The start page `/`, a child of the protected layout. The battery view is
// the start page: `/` opens `/battery` with the default range, without a
// new history entry. The protected layout checks the session first, so a
// request without a session opens the login page with `/` as the target.
import { createRoute, redirect } from '@tanstack/react-router';

import { DEFAULT_RANGE } from '../battery/range.js';
import { protectedRoute } from './protected.js';

export const indexRoute = createRoute({
  getParentRoute: () => protectedRoute,
  path: '/',
  beforeLoad: () => {
    // `throw: true` makes TanStack Router throw the redirect itself.
    redirect({ to: '/battery', search: { range: DEFAULT_RANGE }, replace: true, throw: true });
  },
});
