// The consumption view "Kulutus": the drives of a range, Wh/km against the
// outside temperature and the mean speed, a table of the drives, and the
// totals. Each state has a `data-state` attribute: loading, unavailable,
// no_drives, data, and stale_usable (earlier data after a failed refresh).
import '../battery/battery.css';
import './consumption.css';

import { useQuery } from '@tanstack/react-query';
import type { ConsumptionView, Drive, RangeKey } from 'telemetry-web-api-client';

import { FailureNotice } from '../battery/BatteryPage.js';
import { RangeSelector } from '../battery/RangeSelector.js';
import { ScatterChart } from '../chart/ScatterChart.js';
import type { ScatterPoint } from '../chart/ScatterChart.js';
import { formatDateTimeNear, formatNumber } from '../format/format.js';
import { fi } from '../i18n/fi.js';
import { consumptionQuery } from './consumption-query.js';
import {
  consumptionStatusLine,
  lossesLine,
  noDrivesText,
  scatterSummary,
  driveCells,
  hasUnknownValues,
  refreshStatus,
  toConsumptionViewState,
  totalsText,
} from './view-state.js';
import type { ConsumptionViewState } from './view-state.js';

export function ConsumptionPage({ range }: { readonly range: RangeKey }) {
  const query = useQuery(consumptionQuery(range));
  const state = toConsumptionViewState(query);
  return (
    <section className="battery consumption" aria-labelledby="consumption-title" data-state={state.kind}>
      <div className="battery-head">
        <h1 id="consumption-title">{fi.consumptionTitle}</h1>
        <div className="battery-refresh">
          <p className="battery-fetched" role="status" data-testid="fetched-at">
            {refreshStatus(state, query.isFetching)}
          </p>
          <button
            type="button"
            className="battery-refresh-button"
            aria-disabled={query.isFetching ? true : undefined}
            onClick={() => {
              if (!query.isFetching) {
                void query.refetch();
              }
            }}
          >
            {fi.refresh}
          </button>
        </div>
      </div>
      <RangeSelector current={range} to="/consumption" />
      <StateBody state={state} />
    </section>
  );
}

function StateBody({ state }: { readonly state: ConsumptionViewState }) {
  switch (state.kind) {
    case 'loading':
      return <div className="chart-placeholder chart-placeholder-large" aria-hidden="true" />;
    case 'unavailable':
      return <FailureNotice reason={state.reason} title={fi.unavailableTitle} />;
    case 'stale_usable':
      return (
        <>
          <FailureNotice reason={state.reason} title={fi.staleUsableTitle} />
          {state.view.drives.length === 0 ? <NoDrives view={state.view} /> : <DataBody view={state.view} />}
        </>
      );
    case 'no_drives':
      return <NoDrives view={state.view} />;
    case 'data':
      return <DataBody view={state.view} />;
    default:
      return null;
  }
}

function NoDrives({ view }: { readonly view: ConsumptionView }) {
  return (
    <>
      <StatusLine view={view} />
      <div className="notice notice-info" data-testid="no-drives">
        <p className="notice-title">{noDrivesText(view)}</p>
      </div>
    </>
  );
}

function StatusLine({ view }: { readonly view: ConsumptionView }) {
  const losses = lossesLine(view);
  return (
    <>
      <p className="status-line" data-testid="status-line" data-freshness={view.status?.freshness ?? 'none'}>
        {consumptionStatusLine(view)}
      </p>
      {losses === null ? null : <p data-testid="losses-line">{losses}</p>}
    </>
  );
}

type MarkedPoint = ScatterPoint & { readonly marked: boolean };

function DataBody({ view }: { readonly view: ConsumptionView }) {
  const generatedAtMs = Date.parse(view.generated_at);
  // A drive with a loss mark gets the mark of its table row in its point label.
  const label = (drive: Drive, wh: number, other: string): string => {
    const text = `${formatDateTimeNear(Date.parse(drive.start), generatedAtMs)}: ${formatNumber(wh, 0)} Wh/km, ${other}`;
    return drive.data_loss ? `${text} (${fi.driveDataLoss})` : text;
  };
  const byTemp: MarkedPoint[] = [];
  const bySpeed: MarkedPoint[] = [];
  for (const drive of view.drives) {
    if (drive.consumption === null) {
      continue;
    }
    if (drive.mean_outside_temp !== null) {
      byTemp.push({
        id: drive.start,
        x: drive.mean_outside_temp,
        y: drive.consumption,
        label: label(drive, drive.consumption, `${formatNumber(drive.mean_outside_temp, 1)} °C`),
        marked: drive.data_loss,
      });
    }
    if (drive.mean_speed !== null) {
      bySpeed.push({
        id: drive.start,
        x: drive.mean_speed,
        y: drive.consumption,
        label: label(drive, drive.consumption, `${formatNumber(drive.mean_speed, 0)} km/h`),
        marked: drive.data_loss,
      });
    }
  }
  const ended = view.drives.filter((drive) => drive.end !== null).length;
  return (
    <div className="consumption-body" data-testid="consumption-data" data-partial={hasUnknownValues(view)}>
      <StatusLine view={view} />
      <div className="consumption-totals" data-testid="totals">
        {totalsText(view).map((sentence) => (
          <p key={sentence}>{sentence}</p>
        ))}
      </div>
      <div className="battery-charts">
        <ScatterChart
          title={fi.scatterTempTitle}
          xLabel={fi.scatterTempAxis}
          yLabel="Wh/km"
          points={byTemp}
          summary={scatterSummary(byTemp.length, ended, byTemp.filter((p) => p.marked).length)}
        />
        <ScatterChart
          title={fi.scatterSpeedTitle}
          xLabel={fi.scatterSpeedAxis}
          yLabel="Wh/km"
          points={bySpeed}
          summary={scatterSummary(bySpeed.length, ended, bySpeed.filter((p) => p.marked).length)}
        />
      </div>
      <h2 className="consumption-table-title">{fi.drivesTitle}</h2>
      <div className="table-scroll" tabIndex={0} role="region" aria-label={fi.drivesTitle}>
        <table data-testid="drives-table">
          <thead>
            <tr>
              {fi.driveColumns.map((column) => (
                <th key={column} scope="col">
                  {column}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {view.drives.map((drive) => (
              <tr key={drive.start}>
                {driveCells(drive, generatedAtMs).map((cell, index) => (
                  // oxlint-disable-next-line react/no-array-index-key -- the columns have a fixed order.
                  <td key={index}>{cell}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
