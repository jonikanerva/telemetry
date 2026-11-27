// The start page `/`. It shows the state of the service. The battery view
// replaces it as the start page (`/` redirects to `/battery`, design §7).
import { createRoute } from '@tanstack/react-router';

import { HealthStatus } from '../health/HealthStatus.js';
import { fi } from '../i18n/fi.js';
import { rootRoute } from './root.js';

export const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: HomePage,
});

function HomePage() {
  return (
    <section aria-labelledby="home-title">
      <h1 id="home-title">{fi.appName}</h1>
      <HealthStatus />
    </section>
  );
}
