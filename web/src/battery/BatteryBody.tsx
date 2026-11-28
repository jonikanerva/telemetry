// The body of the battery view with data: the status line, the range
// selector, the chart of the charge level, the chart of the capacity, the
// recorded losses, and the starts of the ingest process. Also the
// placeholder of the loading state, which keeps the same layout so that
// the arrival of the data moves no content.
import type { CapacityChange, Gap, RangeKey } from 'telemetry-web-api-client';

import { TimeSeriesChart } from '../chart/TimeSeriesChart.js';
import { formatDateTimeNear, formatDuration, formatNumber, formatSigned, unitSymbol } from '../format/format.js';
import { fi } from '../i18n/fi.js';
import { RangeSelector } from './RangeSelector.js';
import { statusLine } from './view-state.js';
import type { AvailableView } from './view-state.js';

export function DataBody({ view, range }: { readonly view: AvailableView; readonly range: RangeKey }) {
  const generatedAtMs = Date.parse(view.generated_at);
  const fromMs = Date.parse(view.range.from);
  const toMs = Date.parse(view.range.to);
  const gaps = view.gaps.items;
  const restarts = view.restarts.items;
  const capacity = view.tiles.nominal_full_pack_energy;
  return (
    <div
      className="battery-body"
      data-testid="battery-data"
      data-freshness={view.status.freshness}
      data-connection={view.status.connection.state}
      data-gaps={gaps.length}
    >
      <p className="status-line" data-testid="status-line">
        {statusLine(view)}
      </p>
      <RangeSelector current={range} />
      <div className="battery-charts">
        <TimeSeriesChart
          title={fi.chartSocTitle}
          subtitle={fi.chartSocSubtitle}
          series={view.charts.soc}
          gaps={gaps}
          restarts={restarts}
          fromMs={fromMs}
          toMs={toMs}
          bucketSeconds={view.range.bucket_seconds}
          generatedAtMs={generatedAtMs}
          fixedDomain={[0, 100]}
          tone="primary"
          size="large"
        />
        <TimeSeriesChart
          title={fi.chartCapacityTitle}
          subtitle={capacityText(capacity.change, capacity.reading.unit.symbol)}
          series={view.charts.nominal_full_pack_energy}
          gaps={gaps}
          restarts={restarts}
          fromMs={fromMs}
          toMs={toMs}
          bucketSeconds={view.range.bucket_seconds}
          generatedAtMs={generatedAtMs}
          fixedDomain={null}
          tone="secondary"
          size="small"
        />
      </div>
      <GapList gaps={gaps} truncated={view.gaps.truncated} generatedAtMs={generatedAtMs} />
      <RestartList restarts={restarts} truncated={view.restarts.truncated} generatedAtMs={generatedAtMs} />
    </div>
  );
}

/** The subtitle of the capacity chart, with the change over the range when the API gives it. */
function capacityText(change: CapacityChange, symbol: Parameters<typeof unitSymbol>[0]): string {
  if (change.state !== 'value') {
    return fi.chartCapacitySubtitle;
  }
  return `${fi.chartCapacitySubtitle}. ${fi.capacityChange(`${formatSigned(change.delta)} ${unitSymbol(symbol)}`)}`;
}

function GapList({
  gaps,
  truncated,
  generatedAtMs,
}: {
  readonly gaps: readonly Gap[];
  readonly truncated: boolean;
  readonly generatedAtMs: number;
}) {
  return (
    <section className="events" aria-labelledby="gaps-title" data-testid="gap-list">
      <h2 id="gaps-title">{fi.gapsTitle}</h2>
      <p className="events-note">{fi.gapsClockNote}</p>
      {truncated ? (
        <p className="events-note" data-testid="gaps-truncated">
          {fi.gapsTruncated}
        </p>
      ) : null}
      {gaps.length === 0 ? (
        <p>{fi.gapsNone}</p>
      ) : (
        <div className="table-scroll" tabIndex={0} role="region" aria-label={fi.gapsTableLabel}>
          <table>
            <thead>
              <tr>
                <th scope="col">{fi.gapStart}</th>
                <th scope="col">{fi.gapDuration}</th>
                <th scope="col">{fi.gapReason}</th>
                <th scope="col">{fi.gapCount}</th>
              </tr>
            </thead>
            <tbody>
              {gaps.map((gap) => (
                <tr key={`${gap.kind}${gap.from}${gap.to}`} data-testid="gap-row">
                  <td>{formatDateTimeNear(Date.parse(gap.from), generatedAtMs)}</td>
                  <td>{formatDuration(Date.parse(gap.to) - Date.parse(gap.from))}</td>
                  <td>{fi.gapKinds[gap.kind]}</td>
                  <td>{fi.gapCountUnits[gap.count.of](formatNumber(gap.count.value, 0))}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function RestartList({
  restarts,
  truncated,
  generatedAtMs,
}: {
  readonly restarts: readonly string[];
  readonly truncated: boolean;
  readonly generatedAtMs: number;
}) {
  return (
    <section className="events" aria-labelledby="restarts-title" data-testid="restart-list">
      <h2 id="restarts-title">{fi.restartsTitle}</h2>
      <p className="events-note">{fi.restartsNote}</p>
      {truncated ? <p className="events-note">{fi.restartsTruncated}</p> : null}
      {restarts.length === 0 ? (
        <p>{fi.restartsNone}</p>
      ) : (
        <ul className="restart-list">
          {restarts.map((at) => (
            <li key={at} data-testid="restart-item">
              {formatDateTimeNear(Date.parse(at), generatedAtMs)}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * The placeholder of the loading state: the same blocks as the data,
 * without values. The range selector stays usable while the data loads.
 */
export function LoadingBody({ range }: { readonly range: RangeKey }) {
  return (
    <div className="battery-body battery-placeholder">
      <div className="status-line status-line-placeholder" aria-hidden="true" />
      <RangeSelector current={range} />
      <div className="battery-charts" aria-hidden="true">
        <div className="chart-placeholder chart-placeholder-large" />
        <div className="chart-placeholder chart-placeholder-small" />
      </div>
    </div>
  );
}
