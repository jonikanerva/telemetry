// The login page `/login`. The search value `redirect` is the page to open
// after the login; it is validated to an internal path (rule:
// ../auth/safe-redirect.ts). A browser that already has a cached session
// goes straight to that page.
import { createRoute, redirect } from '@tanstack/react-router';
import type { Session } from 'telemetry-web-api-client';

import { LoginForm } from '../auth/LoginForm.js';
import { HOME_PATH, parseLoginSearch } from '../auth/safe-redirect.js';
import { SESSION_QUERY_KEY } from '../auth/session.js';
import { rootRoute } from './root.js';

export const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/login',
  validateSearch: (search: Record<string, unknown>) => parseLoginSearch(search, window.location.origin),
  beforeLoad: ({ context, search }) => {
    if (context.queryClient.getQueryData<Session>(SESSION_QUERY_KEY) !== undefined) {
      // `throw: true` makes TanStack Router throw the redirect itself.
      redirect({ href: search.redirect ?? HOME_PATH, replace: true, throw: true });
    }
  },
  component: LoginPage,
});

function LoginPage() {
  const search = loginRoute.useSearch();
  return <LoginForm redirect={search.redirect ?? HOME_PATH} />;
}
