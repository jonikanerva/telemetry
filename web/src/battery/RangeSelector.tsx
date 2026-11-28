// The range selector: one link for each range. A link changes the URL,
// so the browser history and the back button hold the range.
import { Link } from '@tanstack/react-router';
import type { RangeKey } from 'telemetry-web-api-client';

import { fi } from '../i18n/fi.js';
import { RANGE_KEYS } from './range.js';

export function RangeSelector({ current }: { readonly current: RangeKey }) {
  return (
    <nav className="range-selector" aria-label={fi.rangeLabel}>
      <ul>
        {RANGE_KEYS.map((key) => (
          <li key={key}>
            <Link
              to="/battery"
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
