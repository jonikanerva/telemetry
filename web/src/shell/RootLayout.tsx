// The layout of every page: a skip link, the header, the main landmark,
// and the footer with the time zone of all times.
import { Outlet } from '@tanstack/react-router';

import { fi } from '../i18n/fi.js';
import { ReloadBanner } from './ReloadBanner.js';

export function RootLayout() {
  return (
    <>
      <a className="skip-link" href="#main">
        {fi.skipToContent}
      </a>
      <header className="app-header">
        <span className="app-name">{fi.appName}</span>
        {/* Mount point: S2 adds the logout control here. S4 adds the navigation. */}
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
