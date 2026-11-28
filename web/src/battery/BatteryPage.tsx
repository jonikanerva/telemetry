// The battery view "Akku" (VISION.md, view 1).
//
// The component renders one typed view state (view-state.ts). Each state
// of VISION.md has its own rendering and a `data-state` attribute:
// loading, unavailable, no data yet, data (with the gap, asleep, and
// stale branches), and stale usable data after a failed refresh.
import './battery.css';

import { useQuery } from '@tanstack/react-query';
import type { BatteryView, RangeKey } from 'telemetry-web-api-client';

import { fi } from '../i18n/fi.js';
import { batteryQuery } from './battery-query.js';
import { DataBody, LoadingBody } from './BatteryBody.js';
import { RangeSelector } from './RangeSelector.js';
import { refreshStatus, toBatteryViewState } from './view-state.js';
import type { BatteryViewState, FailureReason } from './view-state.js';

export function BatteryPage({ range }: { readonly range: RangeKey }) {
  const query = useQuery(batteryQuery(range));
  const state = toBatteryViewState(query);

  return (
    <section className="battery" aria-labelledby="battery-title" data-state={state.kind}>
      <div className="battery-head">
        <h1 id="battery-title">{fi.batteryTitle}</h1>
        <div className="battery-refresh">
          {/* The one live region of the view: the outcome of each fetch (view-state.ts). */}
          <p className="battery-fetched" role="status" data-testid="fetched-at">
            {refreshStatus(state, query.isFetching)}
          </p>
          {/* `aria-disabled`, not `disabled`: a disabled button loses the
              focus (WebKit moves it to the body), and the outcome would then
              reach a keyboard user without the focus on the control. */}
          <button
            type="button"
            className="battery-refresh-button"
            aria-disabled={query.isFetching ? true : undefined}
            onClick={() => {
              if (query.isFetching) {
                return;
              }
              void query.refetch();
            }}
          >
            {fi.refresh}
          </button>
        </div>
      </div>
      <StateBody state={state} range={range} />
    </section>
  );
}

function StateBody({ state, range }: { readonly state: BatteryViewState; readonly range: RangeKey }) {
  switch (state.kind) {
    case 'loading':
      return <LoadingBody range={range} />;
    case 'unavailable':
      return (
        <>
          <FailureNotice reason={state.reason} title={fi.unavailableTitle} />
          <RangeSelector current={range} />
        </>
      );
    case 'stale_usable':
      return (
        <>
          <FailureNotice reason={state.reason} title={fi.staleUsableTitle} />
          <ViewBody view={state.view} range={range} />
        </>
      );
    case 'no_data_yet':
    case 'data':
      return <ViewBody view={state.view} range={range} />;
    default:
      return null;
  }
}

function ViewBody({ view, range }: { readonly view: BatteryView; readonly range: RangeKey }) {
  if (view.state === 'no_data_yet') {
    return (
      <>
        <div className="notice notice-info" data-testid="no-data-yet">
          <p className="notice-title">{fi.noDataYetTitle}</p>
          <p>{fi.noDataYetBody}</p>
        </div>
        <RangeSelector current={range} />
      </>
    );
  }
  return <DataBody view={view} range={range} />;
}

// Not a live region: the status line above announces the outcome
// (refreshStatus), so the failure is announced once.
function FailureNotice({ reason, title }: { readonly reason: FailureReason; readonly title: string }) {
  return (
    <div className="notice notice-error" data-testid="failure" data-reason={reason}>
      <p className="notice-title">{title}</p>
      <p>{fi.failures[reason]}</p>
      {reason === 'invalid_response' || reason === 'rejected_request' ? (
        <button
          type="button"
          onClick={() => {
            window.location.reload();
          }}
        >
          {fi.reloadButton}
        </button>
      ) : null}
    </div>
  );
}
