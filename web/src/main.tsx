// The entry point of the SPA.
import './styles/tokens.css';
import './styles/app.css';

import { QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { LOGIN_PATH } from './auth/safe-redirect.js';
import { createSessionLossHandler } from './auth/session-loss.js';
import { createPageShowHandler } from './page-show.js';
import { createQueryClient } from './query-client.js';
import type { CacheErrorHandler } from './query-client.js';
import { createAppRouter } from './router.js';

const container = document.getElementById('root');
if (container === null) {
  throw new Error('index.html has no #root element');
}

// The session-loss handler needs the router, and the router needs the
// query client. The cache calls the handler only after a request, when
// both exist.
let onSessionLoss: CacheErrorHandler = () => undefined;
const queryClient = createQueryClient({
  onError: (error, meta) => {
    onSessionLoss(error, meta);
  },
});
const router = createAppRouter(queryClient);
onSessionLoss = createSessionLossHandler({
  queryClient,
  origin: window.location.origin,
  currentHref: () => router.latestLocation.href,
  navigateToLogin: (redirect) => {
    void router.navigate({ to: LOGIN_PATH, search: { redirect } });
  },
});

// A page that the back-forward cache restores can show protected data of
// an ended session (rule: page-show.ts).
window.addEventListener(
  'pageshow',
  createPageShowHandler(() => {
    window.location.reload();
  }),
);

createRoot(container).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
