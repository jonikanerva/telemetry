// The start page `/`, a child of the protected layout. It shows the state
// of the service and the end of the session. The battery view replaces it
// as the start page (`/` redirects to `/battery`, design §7).
import { createRoute } from '@tanstack/react-router';

import { formatSessionEnd } from '../auth/session-end.js';
import { useSession } from '../auth/session-snapshot.js';
import { HealthStatus } from '../health/HealthStatus.js';
import { fi } from '../i18n/fi.js';
import { protectedRoute } from './protected.js';

export const indexRoute = createRoute({
  getParentRoute: () => protectedRoute,
  path: '/',
  component: HomePage,
});

function HomePage() {
  return (
    // No `aria-labelledby`: with a named region, axe crashed the Firefox
    // content process of the browser tests (see auth/LoginFormView.tsx).
    <section>
      <h1 id="home-title">{fi.appName}</h1>
      <HealthStatus />
      <SessionEnd />
    </section>
  );
}

function SessionEnd() {
  const session = useSession();
  const end = session === undefined ? null : formatSessionEnd(session.expires_at);
  return end === null ? null : <p className="session-end">{fi.sessionEnds(end)}</p>;
}
