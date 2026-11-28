// A scatter chart in hand-written SVG: one point for each item, for
// example Wh/km against the outside temperature of each drive.
//
// Accessibility: the chart is a `figure` with a caption and a text
// summary. A native range input over the points, ordered by x, is the
// keyboard cursor; its `aria-valuetext` is the readout of the point. The
// table of the page holds every value, so the chart has no table of its
// own. No inline style (CSP): attributes and class names only.
import './chart.css';

import { useEffect, useId, useMemo, useRef, useState } from 'react';

import { formatNumber } from '../format/format.js';
import { fi } from '../i18n/fi.js';
import { linearScale, valueDomain } from './geometry.js';
import type { Box } from './geometry.js';

export type ScatterPoint = {
  /** A stable key, for example the start of the drive. */
  readonly id: string;
  readonly x: number;
  readonly y: number;
  /** The readout of the point, for the cursor and the screen reader. */
  readonly label: string;
};

export type ScatterChartProps = {
  readonly title: string;
  readonly xLabel: string;
  readonly yLabel: string;
  readonly points: readonly ScatterPoint[];
  /** The sentences of the text summary. */
  readonly summary: readonly string[];
};

const MARGIN = { left: 48, right: 12, top: 28, bottom: 40 } as const;
/** The height of the SVG; `.chart-frame-scatter` in chart.css has the same value. */
const HEIGHT = 260;

export function ScatterChart({ title, xLabel, yLabel, points, summary }: ScatterChartProps) {
  const baseId = useId().replace(/[^A-Za-z0-9_-]/g, '');
  const titleId = `${baseId}-title`;
  const summaryId = `${baseId}-summary`;
  const frameRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);

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

  const ordered = useMemo(() => [...points].sort((a, b) => a.x - b.x || a.y - b.y), [points]);
  const geometry = useMemo(() => {
    if (width <= 0) {
      return null;
    }
    const plot: Box = {
      left: MARGIN.left,
      top: MARGIN.top,
      width: Math.max(1, width - MARGIN.left - MARGIN.right),
      height: HEIGHT - MARGIN.top - MARGIN.bottom,
    };
    const xs = valueDomain(
      ordered.map((p) => p.x),
      null,
      Math.max(2, Math.floor(plot.width / 80)),
    );
    const ys = valueDomain(
      ordered.map((p) => p.y),
      null,
    );
    return {
      plot,
      x: linearScale(xs.domain, [plot.left, plot.left + plot.width]),
      y: linearScale(ys.domain, [plot.top + plot.height, plot.top]),
      xTicks: xs.ticks,
      yTicks: ys.ticks,
    };
  }, [width, ordered]);

  const selectedIndex = ordered.findIndex((p) => p.id === selectedId);
  const selected = selectedIndex >= 0 ? ordered[selectedIndex] : undefined;
  const cursorIndex = selectedIndex >= 0 ? selectedIndex : 0;
  const empty = ordered.length === 0;

  return (
    <figure className="chart" aria-labelledby={titleId} aria-describedby={summaryId}>
      <figcaption className="chart-caption">
        <h2 id={titleId} className="chart-title">
          {title}
        </h2>
      </figcaption>
      <p className="chart-readout" data-testid="scatter-readout">
        {selected === undefined ? (empty ? ' ' : fi.scatterCursorHint) : selected.label}
      </p>
      <div className="chart-frame chart-frame-scatter" ref={frameRef}>
        {geometry === null ? null : (
          <svg className="chart-svg" width={width} height={HEIGHT} role="img" aria-label={title}>
            {(empty ? [] : geometry.yTicks).map((value) => (
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
                  {formatNumber(value, 0)}
                </text>
              </g>
            ))}
            {(empty ? [] : geometry.xTicks).map((value) => (
              <text
                key={`x${String(value)}`}
                className="chart-axis-text"
                x={geometry.x.at(value)}
                y={HEIGHT - 24}
                textAnchor="middle"
              >
                {formatNumber(value, 1)}
              </text>
            ))}
            <text
              className="chart-axis-text"
              x={geometry.plot.left + geometry.plot.width / 2}
              y={HEIGHT - 6}
              textAnchor="middle"
            >
              {xLabel}
            </text>
            <text className="chart-axis-text" x={4} y={12} textAnchor="start">
              {yLabel}
            </text>
            {empty ? (
              <text
                className="chart-empty-text"
                x={geometry.plot.left + geometry.plot.width / 2}
                y={geometry.plot.top + geometry.plot.height / 2}
                textAnchor="middle"
              >
                {fi.scatterEmpty}
              </text>
            ) : null}
            {ordered.map((point) => (
              <circle
                key={point.id}
                className="chart-marker chart-marker-primary"
                data-testid="scatter-point"
                cx={geometry.x.at(point.x)}
                cy={geometry.y.at(point.y)}
                r={4}
                onPointerEnter={() => {
                  setSelectedId(point.id);
                }}
              >
                <title>{point.label}</title>
              </circle>
            ))}
            {selected === undefined ? null : (
              <circle
                className="chart-focus-dot chart-marker-primary"
                cx={geometry.x.at(selected.x)}
                cy={geometry.y.at(selected.y)}
                r={7}
              />
            )}
          </svg>
        )}
        {empty ? null : (
          <input
            className="chart-cursor"
            type="range"
            min={0}
            max={ordered.length - 1}
            step={1}
            value={cursorIndex}
            aria-label={fi.chartCursorLabel(title)}
            aria-valuetext={ordered[cursorIndex]?.label ?? ''}
            onFocus={() => {
              if (selected === undefined) {
                setSelectedId(ordered[0]?.id ?? null);
              }
            }}
            onChange={(event) => {
              setSelectedId(ordered[Number.parseInt(event.currentTarget.value, 10)]?.id ?? null);
            }}
            onKeyDown={(event) => {
              if (event.key === 'Escape') {
                setSelectedId(null);
              }
            }}
          />
        )}
      </div>
      <div id={summaryId} className="chart-summary" data-testid="scatter-summary">
        {summary.map((sentence) => (
          <p key={sentence}>{sentence}</p>
        ))}
      </div>
    </figure>
  );
}
