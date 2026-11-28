// The data table of a chart, inside a `details` element. The table
// renders only while the element is open, so a long series costs nothing
// until the owner asks for it. A row marks each place where the line
// breaks at a gap.
import { Fragment, useState } from 'react';
import type { Series, Unit } from 'telemetry-web-api-client';

import { formatDateTimeNear, formatNumber, unitSymbol } from '../format/format.js';
import { fi } from '../i18n/fi.js';

export type ChartTableProps = {
  readonly series: Series;
  readonly unit: Unit;
  readonly showBucket: boolean;
  readonly generatedAtMs: number;
};

/** The table shows the values with up to three decimals, closer to the stored value than the chart. */
const TABLE_DIGITS = 3;

export function ChartTable({ series, unit, showBucket, generatedAtMs }: ChartTableProps) {
  const [open, setOpen] = useState(false);
  const symbol = unitSymbol(unit.symbol);
  const columns = showBucket ? 5 : 2;
  return (
    <details
      className="chart-table"
      onToggle={(event) => {
        setOpen(event.currentTarget.open);
      }}
    >
      <summary>{fi.tableShow}</summary>
      {open ? (
        <div className="table-scroll" tabIndex={0} role="region" aria-label={fi.tableShow}>
          <table>
            <thead>
              <tr>
                <th scope="col">{fi.tableTime}</th>
                <th scope="col">{`${fi.tableValue} (${symbol})`}</th>
                {showBucket ? (
                  <>
                    <th scope="col">{`${fi.tableMin} (${symbol})`}</th>
                    <th scope="col">{`${fi.tableMax} (${symbol})`}</th>
                    <th scope="col">{fi.tableCount}</th>
                  </>
                ) : null}
              </tr>
            </thead>
            <tbody>
              {series.segments.map((segment, segmentIndex) => (
                <Fragment key={segment[0]?.at ?? String(segmentIndex)}>
                  {segmentIndex > 0 ? (
                    <tr className="table-break">
                      <td colSpan={columns}>{fi.tableSegmentBreak}</td>
                    </tr>
                  ) : null}
                  {segment.map((point) => (
                    <tr key={point.at}>
                      <td>{formatDateTimeNear(Date.parse(point.at), generatedAtMs)}</td>
                      <td>{formatNumber(point.last, TABLE_DIGITS)}</td>
                      {showBucket ? (
                        <>
                          <td>{formatNumber(point.min, TABLE_DIGITS)}</td>
                          <td>{formatNumber(point.max, TABLE_DIGITS)}</td>
                          <td>{formatNumber(point.count, 0)}</td>
                        </>
                      ) : null}
                    </tr>
                  ))}
                </Fragment>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </details>
  );
}
