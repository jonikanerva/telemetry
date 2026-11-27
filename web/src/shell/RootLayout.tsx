// The layout of every page: a skip link, the header with the logout
// button while a session exists, the main landmark, and the footer with
// the time zone of all times.
import { Outlet } from '@tanstack/react-router';

import { LogoutButton } from '../auth/LogoutButton.js';
import { useSession } from '../auth/session-snapshot.js';
import { fi } from '../i18n/fi.js';
import { ReloadBanner } from './ReloadBanner.js';

export function RootLayout() {
  // The cached session body only; the layout sends no request.
  const session = useSession();
  return (
    <>
      <a className="skip-link" href="#main">
        {fi.skipToContent}
      </a>
      <header className="app-header">
        <span className="app-name">{fi.appName}</span>
        {/* Mount point: S4 adds the navigation here. */}
        {session === undefined ? null : <LogoutButton />}
      </header>
      <ReloadBanner />
      <main id="main" className="app-main" tabIndex={-1}>
        <Outlet />
      </main>
      <footer className="app-footer">
        <p>{fi.footerTimeZone}</p>
      </footer>
    </>
  );
}
