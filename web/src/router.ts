// The route tree of the SPA. Routes are code-based: each file in routes/
// exports one route, and this file joins them.
//
// Tree:
// - `/login` (routes/login.tsx): public.
// - `/logout` (routes/logout.tsx): public; the page after a logout that
//   the server did not confirm.
// - `protected` (routes/protected.tsx): a pathless layout that checks the
//   session. Its children: `/` (routes/index.tsx).
//
// Mount points: S4 adds routes/battery.tsx (`/battery`) as a child of the
// protected layout, with battery/, chart/, and format/.
import type { QueryClient } from '@tanstack/react-query';
import { createRouter } from '@tanstack/react-router';

import { RouteUnavailable } from './auth/RouteUnavailable.js';
import { indexRoute } from './routes/index.js';
import { loginRoute } from './routes/login.js';
import { logoutRoute } from './routes/logout.js';
import { protectedRoute } from './routes/protected.js';
import { rootRoute } from './routes/root.js';

const routeTree = rootRoute.addChildren([loginRoute, logoutRoute, protectedRoute.addChildren([indexRoute])]);

export function createAppRouter(queryClient: QueryClient) {
  return createRouter({
    routeTree,
    context: { queryClient },
    // The default error view of TanStack Router prints the error text with
    // inline styles, which the content security policy blocks.
    defaultErrorComponent: RouteUnavailable,
  });
}

export type AppRouter = ReturnType<typeof createAppRouter>;

declare module '@tanstack/react-router' {
  interface Register {
    router: AppRouter;
  }
}
