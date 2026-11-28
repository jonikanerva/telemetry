// The layout of every page: a skip link, the header, the main landmark,
// and the footer with the time zone of all times. While a session exists,
// the header has the navigation and the logout button.
import { Link, Outlet } from '@tanstack/react-router';

import { LogoutButton } from '../auth/LogoutButton.js';
import { useSession } from '../auth/session-snapshot.js';
import { DEFAULT_RANGE } from '../battery/range.js';
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
        {/* The battery view is the only view now. Add a link only for a view that exists. */}
        {session === undefined ? null : (
          <nav className="app-nav" aria-label={fi.navLabel}>
            <Link
              to="/battery"
              search={{ range: DEFAULT_RANGE }}
              activeOptions={{ includeSearch: false }}
              className="app-nav-link"
            >
              {fi.navBattery}
            </Link>
          </nav>
        )}
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
