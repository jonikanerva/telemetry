// The entry point of the SPA.
import './styles/tokens.css';
import './styles/app.css';

import { QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { createQueryClient } from './query-client.js';
import { router } from './router.js';

const container = document.getElementById('root');
if (container === null) {
  throw new Error('index.html has no #root element');
}

const queryClient = createQueryClient();

createRoot(container).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
