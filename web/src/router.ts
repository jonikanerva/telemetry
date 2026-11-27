// The route tree of the SPA. Routes are code-based: each file in routes/
// exports one route, and this file joins them.
//
// Mount points:
// - S2 adds routes/login.tsx (`/login`) and the session code in auth/.
// - S4 adds routes/battery.tsx (`/battery`), with battery/, chart/, and
//   format/.
import { createRouter } from '@tanstack/react-router';

import { indexRoute } from './routes/index.js';
import { rootRoute } from './routes/root.js';

const routeTree = rootRoute.addChildren([indexRoute]);

export const router = createRouter({ routeTree });

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
