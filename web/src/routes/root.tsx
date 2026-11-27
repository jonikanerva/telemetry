// The root route: the layout of every page and the page for an unknown
// path. The router context holds the query client, so that `beforeLoad`
// of a route can read and fill the cache (routes/protected.tsx).
import type { QueryClient } from '@tanstack/react-query';
import { createRootRouteWithContext } from '@tanstack/react-router';

import { NotFound } from '../shell/NotFound.js';
import { RootLayout } from '../shell/RootLayout.js';

export type RouterContext = {
  readonly queryClient: QueryClient;
};

export const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: RootLayout,
  notFoundComponent: NotFound,
});
