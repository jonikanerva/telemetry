// The range selector: one link for each range. A link changes the URL,
// so the browser history and the back button hold the range. Each view
// that has a range passes its own path.
import { Link } from '@tanstack/react-router';
import type { RangeKey } from 'telemetry-web-api-client';

import { fi } from '../i18n/fi.js';
import { RANGE_KEYS } from './range.js';

export function RangeSelector({
  current,
  to = '/battery',
}: {
  readonly current: RangeKey;
  readonly to?: '/battery' | '/consumption';
}) {
  return (
    <nav className="range-selector" aria-label={fi.rangeLabel}>
      <ul>
        {RANGE_KEYS.map((key) => (
          <li key={key}>
            <Link
              to={to}
              search={{ range: key }}
              aria-current={key === current ? 'page' : undefined}
              className="range-link"
            >
              {fi.rangeNames[key]}
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  );
}
