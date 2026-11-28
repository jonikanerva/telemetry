// The geometry of a time-series chart: scales, ticks, line paths, and gap
// areas. Pure functions with no DOM, so the rules have unit tests.
//
// Rules (contract `Series`, ADR 0013 §5):
// - Each segment of the server is its own line. A line never crosses a
//   gap. A time without samples (the car asleep) does not split a line.
// - The carry-in point (the newest value before the range) joins the
//   first segment unless a gap meets the open interval between them. It
//   lies left of the plot area; the clip path of the chart cuts it.
// - A missing value is never drawn: no point means no mark, never zero.
// - A gap with `from == to` still gets a visible area of `MIN_GAP_WIDTH`.
import type { Gap, Point, Series } from 'telemetry-web-api-client';

import { zonedParts } from '../format/format.js';

export const MIN_GAP_WIDTH = 4;

export type Box = { readonly left: number; readonly top: number; readonly width: number; readonly height: number };

export type LinearScale = {
  readonly domain: readonly [number, number];
  readonly at: (value: number) => number;
};

export function linearScale(domain: readonly [number, number], range: readonly [number, number]): LinearScale {
  const [d0, d1] = domain;
  const [r0, r1] = range;
  const span = d1 - d0;
  return {
    domain,
    at: (value) => (span === 0 ? (r0 + r1) / 2 : r0 + ((value - d0) / span) * (r1 - r0)),
  };
}

/** A tick step of 1, 2, or 5 times a power of ten that gives at most `count` intervals. */
export function niceStep(span: number, count: number): number {
  if (!(span > 0) || count < 1) {
    return 1;
  }
  const raw = span / count;
  const power = 10 ** Math.floor(Math.log10(raw));
  for (const factor of [1, 2, 5, 10]) {
    if (power * factor >= raw) {
      return power * factor;
    }
  }
  return power * 10;
}

/** The values of a series that the chart shows: every min, max, and last, and the carry-in value. */
export function seriesValues(series: Series): number[] {
  const values: number[] = [];
  const add = (point: Point): void => {
    values.push(point.min, point.max, point.last);
  };
  if (series.carry_in !== null) {
    add(series.carry_in);
  }
  for (const segment of series.segments) {
    segment.forEach(add);
  }
  return values;
}

/**
 * The value domain and its ticks. A percent series uses 0 to 100. Another
 * series uses the nice bounds around its values; a single value gets a
 * domain of one step on each side.
 */
export function valueDomain(
  values: readonly number[],
  fixed: readonly [number, number] | null,
  tickCount = 4,
): { readonly domain: readonly [number, number]; readonly ticks: readonly number[] } {
  let low: number;
  let high: number;
  if (fixed !== null) {
    [low, high] = fixed;
  } else if (values.length === 0) {
    [low, high] = [0, 1];
  } else {
    low = Math.min(...values);
    high = Math.max(...values);
    if (low === high) {
      const pad = Math.max(Math.abs(low) * 0.01, 0.5);
      low -= pad;
      high += pad;
    }
    const step = niceStep(high - low, tickCount);
    low = Math.floor(low / step) * step;
    high = Math.ceil(high / step) * step;
  }
  const step = niceStep(high - low, tickCount);
  const ticks: number[] = [];
  for (let value = Math.ceil(low / step) * step; value <= high + step / 1e6; value += step) {
    // Remove the binary rounding noise of the repeated addition.
    ticks.push(Number.parseFloat(value.toPrecision(12)));
  }
  return { domain: [low, high], ticks };
}

/** True when a gap meets the open interval between two instants (epoch ms). */
export function gapBetween(gaps: readonly Gap[], afterMs: number, beforeMs: number): boolean {
  return gaps.some((gap) => Date.parse(gap.from) < beforeMs && Date.parse(gap.to) > afterMs);
}

/** One drawable line: its points in time order. */
export type Line = { readonly points: readonly Point[]; readonly withCarryIn: boolean };

/** The lines of a series, with the carry-in point joined where the rule allows. */
export function seriesLines(series: Series, gaps: readonly Gap[]): Line[] {
  const lines: Line[] = series.segments
    .filter((segment) => segment.length > 0)
    .map((segment) => ({ points: segment, withCarryIn: false }));
  const carry = series.carry_in;
  const first = lines[0];
  const firstPoint = first?.points[0];
  if (carry !== null && first !== undefined && firstPoint !== undefined) {
    if (!gapBetween(gaps, Date.parse(carry.at), Date.parse(firstPoint.at))) {
      lines[0] = { points: [carry, ...first.points], withCarryIn: true };
    }
  }
  return lines;
}

/** The points of the range in time order, for the cursor and the table. The carry-in point is not one. */
export function rangePoints(series: Series): Point[] {
  return series.segments.flat();
}

/** The index of the point nearest to an instant, or -1 for no point. */
export function nearestIndex(points: readonly Point[], atMs: number): number {
  if (points.length === 0) {
    return -1;
  }
  let low = 0;
  let high = points.length - 1;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    const point = points[middle];
    if (point !== undefined && Date.parse(point.at) < atMs) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  const after = points[low];
  const before = points[low - 1];
  if (before !== undefined && after !== undefined && atMs - Date.parse(before.at) <= Date.parse(after.at) - atMs) {
    return low - 1;
  }
  return low;
}

function fixed(value: number): string {
  return value.toFixed(1);
}

/** An SVG path through the `last` values of the points. */
export function linePath(points: readonly Point[], x: LinearScale, y: LinearScale): string {
  return points
    .map((point, index) => `${index === 0 ? 'M' : 'L'}${fixed(x.at(Date.parse(point.at)))},${fixed(y.at(point.last))}`)
    .join('');
}

export type GapArea = { readonly gap: Gap; readonly x: number; readonly width: number };

/** The areas of the gaps inside the plot, clamped to the range, each at least `MIN_GAP_WIDTH` wide. */
export function gapAreas(gaps: readonly Gap[], fromMs: number, toMs: number, x: LinearScale, plot: Box): GapArea[] {
  const areas: GapArea[] = [];
  for (const gap of gaps) {
    const start = Math.max(Date.parse(gap.from), fromMs);
    const end = Math.min(Date.parse(gap.to), toMs);
    if (end < start) {
      continue;
    }
    let x0 = x.at(start);
    let width = x.at(end) - x0;
    if (width < MIN_GAP_WIDTH) {
      x0 -= (MIN_GAP_WIDTH - width) / 2;
      width = MIN_GAP_WIDTH;
    }
    x0 = Math.min(Math.max(x0, plot.left), plot.left + plot.width - width);
    areas.push({ gap, x: x0, width });
  }
  return areas;
}

export type TimeTick = { readonly at: number; readonly kind: 'hour' | 'day' | 'month' };

// The time ticks use fixed hours only to make candidate instants. Each
// label and each "midnight" comes from the calendar parts of
// Europe/Helsinki (zonedParts): the offsets of the zone are whole hours,
// so each local whole hour is a UTC whole hour, and a local midnight is
// at 21:00 or 22:00 UTC (no change of summer time happens at midnight).
const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;

/** The instants of local midnight in Europe/Helsinki in `[fromMs, toMs]`. */
export function helsinkiMidnights(fromMs: number, toMs: number): number[] {
  const result: number[] = [];
  const firstDay = Math.floor(fromMs / DAY_MS) - 1;
  const lastDay = Math.ceil(toMs / DAY_MS);
  for (let day = firstDay; day <= lastDay; day += 1) {
    // Helsinki is UTC+2 or UTC+3, so its midnight is at 22:00 or 21:00 UTC
    // of the day before.
    for (const hour of [21, 22]) {
      const at = day * DAY_MS + hour * HOUR_MS;
      if (at < fromMs || at > toMs) {
        continue;
      }
      const parts = zonedParts(at);
      if (parts.hour === 0 && parts.minute === 0) {
        result.push(at);
      }
    }
  }
  return result;
}

function hourTicks(fromMs: number, toMs: number, step: number): TimeTick[] {
  const ticks: TimeTick[] = [];
  for (let at = Math.ceil(fromMs / HOUR_MS) * HOUR_MS; at <= toMs; at += HOUR_MS) {
    const parts = zonedParts(at);
    if (parts.minute === 0 && parts.hour % step === 0) {
      ticks.push({ at, kind: parts.hour === 0 ? 'day' : 'hour' });
    }
  }
  return ticks;
}

/**
 * The time ticks of a range, at whole hours, local midnights, or the first
 * days of months in Europe/Helsinki, with at most `maxTicks` ticks.
 */
export function timeTicks(fromMs: number, toMs: number, maxTicks: number): TimeTick[] {
  const limit = Math.max(2, maxTicks);
  if (toMs - fromMs <= 2 * DAY_MS) {
    for (const step of [1, 2, 3, 6, 12]) {
      const ticks = hourTicks(fromMs, toMs, step);
      if (ticks.length <= limit) {
        return ticks;
      }
    }
    return hourTicks(fromMs, toMs, 24);
  }
  const midnights = helsinkiMidnights(fromMs, toMs);
  for (const step of [1, 2, 3, 7, 14]) {
    // Count the step from a fixed day number, so that a tick stays on the
    // same date when the range moves. The number only spaces the ticks; the
    // label of a tick is its Helsinki date.
    const ticks = midnights.filter((at) => Math.round(at / DAY_MS) % step === 0);
    if (ticks.length <= limit) {
      return ticks.map((at) => ({ at, kind: 'day' }));
    }
  }
  const firsts = midnights.filter((at) => zonedParts(at).day === 1);
  for (const step of [1, 2, 3, 6, 12]) {
    const ticks = firsts.filter((at) => (zonedParts(at).month - 1) % step === 0);
    if (ticks.length <= limit) {
      return ticks.map((at) => ({ at, kind: 'month' }));
    }
  }
  return [];
}
