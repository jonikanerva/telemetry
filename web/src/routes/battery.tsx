// The battery view `/battery?range=<key>`.
//
// The URL owns the range (range.ts). A missing or unknown range is
// replaced with the default, without a new history entry.
//
// The route is a child of the protected layout, which checks the session
// before the check of the search below.
import { createRoute, redirect } from '@tanstack/react-router';

import { BatteryPage } from '../battery/BatteryPage.js';
import { isCanonicalSearch, validateBatterySearch } from '../battery/range.js';
import { protectedRoute } from './protected.js';

export const batteryRoute = createRoute({
  getParentRoute: () => protectedRoute,
  path: '/battery',
  validateSearch: validateBatterySearch,
  beforeLoad: ({ location }) => {
    const raw: Record<string, unknown> = location.search;
    if (!isCanonicalSearch(raw)) {
      // oxlint-disable-next-line typescript/only-throw-error -- TanStack Router redirects by a thrown redirect object.
      throw redirect({ to: '/battery', search: validateBatterySearch(raw), replace: true });
    }
  },
  component: BatteryRouteComponent,
});

function BatteryRouteComponent() {
  const { range } = batteryRoute.useSearch();
  return <BatteryPage range={range} />;
}
