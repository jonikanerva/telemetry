// The state of the service from GET /health, as a typed view state.
import { useQuery } from '@tanstack/react-query';

import { fi } from '../i18n/fi.js';
import { toViewState } from '../view-state.js';
import { healthQuery } from './health-query.js';
import { parseVersion } from './version.js';

export function HealthStatus() {
  const query = useQuery(healthQuery);
  const state = toViewState(query);
  return (
    <div className="health">
      <div role="status" aria-live="polite" data-state={state.kind}>
        {state.kind === 'loading' ? <p>{fi.healthLoading}</p> : null}
        {state.kind === 'unavailable' ? (
          <div className="notice notice-error">
            <p className="notice-title">{fi.unavailableTitle}</p>
            <p>{fi.unavailableBody}</p>
          </div>
        ) : null}
        {state.kind === 'data' ? (
          <dl>
            <dt>{fi.homeTitle}</dt>
            <dd>{fi.healthAvailable}</dd>
            <dt>{fi.healthVersion}</dt>
            <dd>
              <VersionText version={state.data.version} />
            </dd>
          </dl>
        ) : null}
      </div>
      <button
        type="button"
        disabled={query.isFetching}
        onClick={() => {
          void query.refetch();
        }}
      >
        {fi.refresh}
      </button>
    </div>
  );
}

function VersionText({ version }: { readonly version: string }) {
  const parsed = parseVersion(version);
  return parsed.kind === 'known' ? <code>{parsed.sha.slice(0, 7)}</code> : <>{fi.versionUnknown}</>;
}
