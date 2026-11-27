// A banner that asks for a page reload when the server runs another
// version than the SPA in this tab (rule: ../health/version.ts).
import { useQuery } from '@tanstack/react-query';

import { healthQuery } from '../health/health-query.js';
import { needsReload, parseVersion, readClientVersion } from '../health/version.js';
import { fi } from '../i18n/fi.js';

const clientVersion = readClientVersion(document);

export function ReloadBanner() {
  const health = useQuery(healthQuery);
  const show = health.data !== undefined && needsReload(clientVersion, parseVersion(health.data.version));
  return (
    <div role="status" aria-live="polite">
      {show ? (
        <div className="banner">
          <p>{fi.reloadBanner}</p>
          <button
            type="button"
            onClick={() => {
              window.location.reload();
            }}
          >
            {fi.reloadButton}
          </button>
        </div>
      ) : null}
    </div>
  );
}
