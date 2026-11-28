// The consumption view `/consumption?range=<key>`. The URL owns the range,
// with the rules of the battery view (battery/range.ts).
import { createRoute, redirect } from '@tanstack/react-router';

import { isCanonicalSearch, validateBatterySearch } from '../battery/range.js';
import { ConsumptionPage } from '../consumption/ConsumptionPage.js';
import { protectedRoute } from './protected.js';

export const consumptionRoute = createRoute({
  getParentRoute: () => protectedRoute,
  path: '/consumption',
  validateSearch: validateBatterySearch,
  beforeLoad: ({ location }) => {
    const raw: Record<string, unknown> = location.search;
    if (!isCanonicalSearch(raw)) {
      // oxlint-disable-next-line typescript/only-throw-error -- TanStack Router redirects by a thrown redirect object.
      throw redirect({ to: '/consumption', search: validateBatterySearch(raw), replace: true });
    }
  },
  component: ConsumptionRouteComponent,
});

function ConsumptionRouteComponent() {
  const { range } = consumptionRoute.useSearch();
  return <ConsumptionPage range={range} />;
}
