// The root route: the layout of every page and the page for an unknown path.
import { createRootRoute } from '@tanstack/react-router';

import { NotFound } from '../shell/NotFound.js';
import { RootLayout } from '../shell/RootLayout.js';

export const rootRoute = createRootRoute({
  component: RootLayout,
  notFoundComponent: NotFound,
});
