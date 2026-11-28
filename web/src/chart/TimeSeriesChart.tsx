// A time-series chart in hand-written SVG: no chart library meets the CSP
// (`style-src 'self'`), the time ticks in Europe/Helsinki, and the bundle budget together.
//
// Accessibility (WCAG 2.2 AA):
// - The chart is a `figure` with a caption, a text summary (min, max,
//   latest, gap count, excluded rows), and a data table in a `details`
//   element.
// - A native range input over the point index is the keyboard cursor. Its
//   `aria-valuetext` is the readout, so a screen reader announces each
//   step. The visible readout above the plot is not a live region, so a
//   pointer move does not flood the screen reader.
// - A gap has a hatch pattern and a legend text, not colour only.
//
// The content security policy allows no inline style: the component sets
// presentation attributes and class names only.
import './chart.css';

import { useEffect, useId, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent, PointerEvent } from 'react';
import type { Gap, Point, Series } from 'telemetry-web-api-client';

import {
  formatDateTimeNear,
  formatDayMonth,
  formatClock,
  formatNumber,
  formatValue,
  formatDuration,
} from '../format/format.js';
import { fi } from '../i18n/fi.js';
import {
  gapAreas,
  linearScale,
  linePath,
  nearestIndex,
  rangePoints,
  seriesLines,
  seriesValues,
  timeTicks,
  valueDomain,
} from './geometry.js';
import type { Box } from './geometry.js';
import { ChartTable } from './ChartTable.js';

/** The number of points up to which every point gets a marker. */
const MARKER_LIMIT = 60;
const MARGIN = { left: 44, right: 12, top: 10, bottom: 26 } as const;

export type TimeSeriesChartProps = {
  readonly title: string;
  readonly subtitle: string;
  readonly series: Series;
  readonly gaps: readonly Gap[];
  /** Ingest clock: the starts of the ingest process, drawn as ticks. */
  readonly restarts: readonly string[];
  readonly fromMs: number;
  readonly toMs: number;
  readonly bucketSeconds: number;
  /** The `generated_at` of the response: the reference of "tänään" and "eilen". */
  readonly generatedAtMs: number;
  /** A fixed value domain, for example 0 to 100 for percent. */
  readonly fixedDomain: readonly [number, number] | null;
  readonly tone: 'primary' | 'secondary';
  readonly size: 'large' | 'small';
};

export function TimeSeriesChart(props: TimeSeriesChartProps) {
  const { title, subtitle, series, gaps, fromMs, toMs, generatedAtMs, bucketSeconds } = props;
  const baseId = useId().replace(/[^A-Za-z0-9_-]/g, '');
  const titleId = `${baseId}-title`;
  const summaryId = `${baseId}-summary`;
  const hatchId = `${baseId}-hatch`;
  const clipId = `${baseId}-clip`;

  const frameRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  // The selection is the time of a point, so a new series drops it.
  const [selectedAt, setSelectedAt] = useState<string | null>(null);
  const pointerKind = useRef<string>('mouse');

  useEffect(() => {
    const frame = frameRef.current;
    if (frame === null) {
      return undefined;
    }
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry !== undefined) {
        setWidth(Math.floor(entry.contentRect.width));
      }
    });
    observer.observe(frame);
    return () => {
      observer.disconnect();
    };
  }, []);

  const points = useMemo(() => rangePoints(series), [series]);
  const lines = useMemo(() => seriesLines(series, gaps), [series, gaps]);
  const unit = series.unit;
  const height = props.size === 'large' ? 220 : 160;

  const geometry = useMemo(() => {
    if (width <= 0) {
      return null;
    }
    const plot: Box = {
      left: MARGIN.left,
      top: MARGIN.top,
      width: Math.max(1, width - MARGIN.left - MARGIN.right),
      height: height - MARGIN.top - MARGIN.bottom,
    };
    const x = linearScale([fromMs, toMs], [plot.left, plot.left + plot.width]);
    const values = valueDomain(seriesValues(series), props.fixedDomain);
    const y = linearScale(values.domain, [plot.top + plot.height, plot.top]);
    const ticks = timeTicks(fromMs, toMs, Math.floor(plot.width / 72));
    const areas = gapAreas(gaps, fromMs, toMs, x, plot);
    return { plot, x, y, yTicks: values.ticks, ticks, areas };
  }, [width, height, fromMs, toMs, series, gaps, props.fixedDomain]);

  const selected = selectedAt === null ? -1 : points.findIndex((point) => point.at === selectedAt);
  const selectedPoint = selected >= 0 ? points[selected] : undefined;
  const setSelected = (index: number): void => {
    setSelectedAt(points[index]?.at ?? null);
  };
  const readoutFor = (point: Point): string => {
    const value = formatValue(point.last, unit);
    const base = fi.chartReadout(formatDateTimeNear(Date.parse(point.at), generatedAtMs), value);
    return point.count > 1
      ? `${base} (${fi.chartReadoutBucket(formatNumber(point.min), formatValue(point.max, unit), point.count)})`
      : base;
  };

  const selectAt = (event: PointerEvent<SVGSVGElement>): void => {
    if (geometry === null || points.length === 0) {
      return;
    }
    const rect = event.currentTarget.getBoundingClientRect();
    const px = event.clientX - rect.left;
    const ratio = (px - geometry.plot.left) / geometry.plot.width;
    const atMs = fromMs + Math.min(1, Math.max(0, ratio)) * (toMs - fromMs);
    setSelected(nearestIndex(points, atMs));
  };

  const onCursorKey = (event: KeyboardEvent<HTMLInputElement>): void => {
    // The range input handles the arrows, Home, End, and Page keys. Escape
    // clears the selection.
    if (event.key === 'Escape') {
      setSelected(-1);
    }
  };

  const showMarkers = points.length <= MARKER_LIMIT;
  const summary = chartSummary(series, points, gaps.length, unit, generatedAtMs);
  const bucketNote = fi.chartBucketNote(formatDuration(bucketSeconds * 1000));
  const noData = points.length === 0;

  return (
    <figure className="chart" aria-labelledby={titleId} aria-describedby={summaryId}>
      <figcaption className="chart-caption">
        <h2 id={titleId} className="chart-title">
          {title}
        </h2>
        <p className="chart-subtitle">{subtitle}</p>
      </figcaption>
      <p className="chart-readout" data-testid="chart-readout">
        {selectedPoint === undefined ? (noData ? ' ' : fi.chartCursorHint) : readoutFor(selectedPoint)}
      </p>
      <div className={`chart-frame chart-frame-${props.size}`} ref={frameRef}>
        {geometry === null ? null : (
          <svg
            className="chart-svg"
            width={width}
            height={height}
            role="img"
            aria-label={title}
            onPointerDown={(event) => {
              pointerKind.current = event.pointerType;
              selectAt(event);
            }}
            onPointerMove={(event) => {
              pointerKind.current = event.pointerType;
              if (event.pointerType === 'mouse' || event.buttons !== 0) {
                selectAt(event);
              }
            }}
            onPointerLeave={() => {
              if (pointerKind.current === 'mouse') {
                setSelected(-1);
              }
            }}
          >
            <defs>
              <pattern id={hatchId} width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
                <rect className="chart-hatch-bg" width="6" height="6" />
                <line className="chart-hatch-line" x1="0" y1="0" x2="0" y2="6" />
              </pattern>
              <clipPath id={clipId}>
                <rect
                  x={geometry.plot.left}
                  y={geometry.plot.top - 4}
                  width={geometry.plot.width}
                  height={geometry.plot.height + 8}
                />
              </clipPath>
            </defs>
            {(noData && props.fixedDomain === null ? [] : geometry.yTicks).map((value) => (
              <g key={`y${String(value)}`}>
                <line
                  className="chart-grid"
                  x1={geometry.plot.left}
                  x2={geometry.plot.left + geometry.plot.width}
                  y1={geometry.y.at(value)}
                  y2={geometry.y.at(value)}
                />
                <text
                  className="chart-axis-text"
                  x={geometry.plot.left - 6}
                  y={geometry.y.at(value) + 4}
                  textAnchor="end"
                >
                  {formatNumber(value, 2)}
                </text>
              </g>
            ))}
            {geometry.ticks.map((tick) => (
              <text
                key={`x${String(tick.at)}`}
                className="chart-axis-text"
                x={geometry.x.at(tick.at)}
                y={height - 8}
                textAnchor="middle"
              >
                {tick.kind === 'hour' ? formatClock(tick.at) : formatDayMonth(tick.at)}
              </text>
            ))}
            {noData ? (
              <text
                className="chart-empty-text"
                x={geometry.plot.left + geometry.plot.width / 2}
                y={geometry.plot.top + geometry.plot.height / 2}
                textAnchor="middle"
              >
                {fi.chartNoDataInRange}
              </text>
            ) : null}
            {geometry.areas.map((area) => (
              <rect
                key={`gap${area.gap.from}${area.gap.to}${area.gap.kind}`}
                className="chart-gap"
                data-testid="chart-gap"
                x={area.x}
                y={geometry.plot.top}
                width={area.width}
                height={geometry.plot.height}
                fill={`url(#${hatchId})`}
              />
            ))}
            {props.restarts.map((at) => {
              const atMs = Date.parse(at);
              if (atMs < fromMs || atMs > toMs) {
                return null;
              }
              const x = geometry.x.at(atMs);
              return (
                <line
                  key={`restart${at}`}
                  className="chart-restart"
                  data-testid="chart-restart"
                  x1={x}
                  x2={x}
                  y1={geometry.plot.top}
                  y2={geometry.plot.top + 8}
                >
                  <title>{fi.chartRestartTitle(formatDateTimeNear(atMs, generatedAtMs))}</title>
                </line>
              );
            })}
            <g clipPath={`url(#${clipId})`}>
              {lines.map((line) =>
                line.points.length > 1 ? (
                  <path
                    key={`line${line.points[0]?.at ?? ''}`}
                    className={`chart-line chart-line-${props.tone}`}
                    data-testid="chart-line"
                    d={linePath(line.points, geometry.x, geometry.y)}
                  />
                ) : null,
              )}
              {points.map((point, index) =>
                showMarkers || isLonePoint(lines, point) ? (
                  <circle
                    key={`m${point.at}`}
                    className={`chart-marker chart-marker-${props.tone}`}
                    data-testid="chart-marker"
                    data-index={index}
                    cx={geometry.x.at(Date.parse(point.at))}
                    cy={geometry.y.at(point.last)}
                    r={3}
                  />
                ) : null,
              )}
            </g>
            {selectedPoint === undefined ? null : (
              <g className="chart-cursor-mark">
                <line
                  className="chart-crosshair"
                  x1={geometry.x.at(Date.parse(selectedPoint.at))}
                  x2={geometry.x.at(Date.parse(selectedPoint.at))}
                  y1={geometry.plot.top}
                  y2={geometry.plot.top + geometry.plot.height}
                />
                <circle
                  className={`chart-focus-dot chart-marker-${props.tone}`}
                  cx={geometry.x.at(Date.parse(selectedPoint.at))}
                  cy={geometry.y.at(selectedPoint.last)}
                  r={5}
                />
              </g>
            )}
          </svg>
        )}
        {noData ? null : (
          <input
            className="chart-cursor"
            type="range"
            min={0}
            max={points.length - 1}
            step={1}
            value={selected < 0 ? points.length - 1 : selected}
            aria-label={fi.chartCursorLabel(title)}
            aria-valuetext={readoutFor(points[selected < 0 ? points.length - 1 : selected] ?? points[0] ?? emptyPoint)}
            onFocus={() => {
              if (selected < 0) {
                setSelected(points.length - 1);
              }
            }}
            onChange={(event) => {
              setSelected(Number.parseInt(event.currentTarget.value, 10));
            }}
            onKeyDown={onCursorKey}
          />
        )}
      </div>
      <ul className="chart-legend">
        <li>
          <LegendSwatch kind={`line-${props.tone}`} />
          {fi.chartLegendLine}
        </li>
        <li>
          <LegendSwatch kind="gap" />
          {fi.chartLegendGap}
        </li>
        <li>
          <LegendSwatch kind="restart" />
          {fi.chartLegendRestart}
        </li>
      </ul>
      <div id={summaryId} className="chart-summary" data-testid="chart-summary">
        {summary.map((sentence) => (
          <p key={sentence}>{sentence}</p>
        ))}
        {noData ? null : <p>{bucketNote}</p>}
      </div>
      {noData ? null : <ChartTable series={series} unit={unit} showBucket generatedAtMs={generatedAtMs} />}
    </figure>
  );
}

const emptyPoint: Point = { at: '1970-01-01T00:00:00Z', last: 0, min: 0, max: 0, count: 0 };

function isLonePoint(lines: readonly { readonly points: readonly Point[] }[], point: Point): boolean {
  return lines.some((line) => line.points.length === 1 && line.points[0] === point);
}

/** The swatch of a legend entry: a small SVG with the same marks as the chart. */
function LegendSwatch({ kind }: { readonly kind: string }) {
  const patternId = `legend-hatch-${useId().replace(/[^A-Za-z0-9_-]/g, '')}`;
  return (
    <svg className="chart-swatch" width="18" height="12" aria-hidden="true" focusable="false">
      {kind === 'gap' ? (
        <>
          <defs>
            <pattern id={patternId} width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
              <rect className="chart-hatch-bg" width="6" height="6" />
              <line className="chart-hatch-line" x1="0" y1="0" x2="0" y2="6" />
            </pattern>
          </defs>
          <rect className="chart-gap" x="0" y="0" width="18" height="12" fill={`url(#${patternId})`} />
        </>
      ) : kind === 'restart' ? (
        <line className="chart-restart" x1="9" y1="1" x2="9" y2="11" />
      ) : (
        <line className={`chart-line chart-line-${kind.slice(5)}`} x1="1" y1="6" x2="17" y2="6" />
      )}
    </svg>
  );
}

/** The sentences of the text summary of a chart. */
export function chartSummary(
  series: Series,
  points: readonly Point[],
  gapCount: number,
  unit: Series['unit'],
  generatedAtMs: number,
): string[] {
  const sentences: string[] = [];
  const latest = points[points.length - 1];
  if (latest === undefined) {
    sentences.push(fi.chartNoDataInRange);
    if (series.carry_in !== null) {
      sentences.push(
        fi.chartCarryIn(
          formatValue(series.carry_in.last, unit),
          formatDateTimeNear(Date.parse(series.carry_in.at), generatedAtMs),
        ),
      );
    }
  } else {
    const min = Math.min(...points.map((point) => point.min));
    const max = Math.max(...points.map((point) => point.max));
    sentences.push(
      fi.chartSummary(
        formatValue(min, unit),
        formatValue(max, unit),
        formatValue(latest.last, unit),
        formatDateTimeNear(Date.parse(latest.at), generatedAtMs),
      ),
    );
  }
  sentences.push(gapCount === 0 ? fi.chartNoGaps : fi.chartGapCount(gapCount));
  if (series.excluded.invalid_marker > 0 || series.excluded.not_numeric > 0) {
    sentences.push(fi.chartExcluded(series.excluded.invalid_marker, series.excluded.not_numeric));
  }
  return sentences;
}
