// The pathless layout route `protected`. Every page that shows owner data
// is its child. `beforeLoad` checks the session before a child renders
// (rule: ../auth/protect.ts): a 401 opens the login page, and any other
// failure shows the unavailable state, never the login page.
import { createRoute } from '@tanstack/react-router';

import { requireSession } from '../auth/protect.js';
import { RouteUnavailable } from '../auth/RouteUnavailable.js';
import { sessionQuery } from '../auth/session.js';
import { rootRoute } from './root.js';

export const protectedRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: 'protected',
  beforeLoad: async ({ context, location }) => {
    await requireSession(context.queryClient, sessionQuery, location.href, window.location.origin);
  },
  errorComponent: RouteUnavailable,
});
