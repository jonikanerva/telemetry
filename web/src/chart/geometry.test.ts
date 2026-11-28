import type { Gap } from 'telemetry-web-api-client';
import { describe, expect, it } from 'vitest';

import { point, series } from '../battery/fixtures.js';
import { zonedParts } from '../format/format.js';
import {
  MIN_GAP_WIDTH,
  gapAreas,
  gapBetween,
  helsinkiMidnights,
  linePath,
  linearScale,
  nearestIndex,
  niceStep,
  rangePoints,
  seriesLines,
  timeTicks,
  valueDomain,
} from './geometry.js';

const at = (iso: string): number => Date.parse(iso);

function gap(from: string, to: string): Gap {
  return { kind: 'queue_full', from, to, count: { value: 1, of: 'records' } };
}

describe('lines', () => {
  const a = point('2026-11-25T10:00:00Z', 70);
  const b = point('2026-11-25T11:00:00Z', 69);
  const c = point('2026-11-26T18:00:00Z', 60);

  it('draws one line for each segment; a long sleep inside a segment does not break it', () => {
    // b and c are 31 hours apart (the car asleep), in one segment.
    const lines = seriesLines(series([[a, b, c]]), []);
    expect(lines).toHaveLength(1);
    expect(lines[0]?.points).toEqual([a, b, c]);
  });

  it('never joins two segments', () => {
    const lines = seriesLines(series([[a], [b, c]]), [gap('2026-11-25T10:30:00Z', '2026-11-25T10:40:00Z')]);
    expect(lines.map((line) => line.points)).toEqual([[a], [b, c]]);
  });

  it('joins the carry-in point to the first segment without a gap between them', () => {
    const carry = point('2026-11-19T10:00:00Z', 80);
    const lines = seriesLines(series([[a, b]], carry), []);
    expect(lines[0]).toEqual({ points: [carry, a, b], withCarryIn: true });
  });

  it('does not join the carry-in point across a gap, also a gap with from = to', () => {
    const carry = point('2026-11-19T10:00:00Z', 80);
    const lines = seriesLines(series([[a, b]], carry), [gap('2026-11-22T00:00:00Z', '2026-11-22T00:00:00Z')]);
    expect(lines[0]).toEqual({ points: [a, b], withCarryIn: false });
  });

  it('draws no line and no zero for an empty range', () => {
    expect(seriesLines(series([], point('2026-11-19T10:00:00Z', 80)), [])).toEqual([]);
    expect(rangePoints(series([]))).toEqual([]);
  });

  it('puts the line through the last values', () => {
    const x = linearScale([at(a.at), at(b.at)], [0, 100]);
    const y = linearScale([0, 100], [100, 0]);
    expect(linePath([a, b], x, y)).toBe('M0.0,30.0L100.0,31.0');
  });
});

describe('gaps', () => {
  const from = at('2026-11-20T12:00:00Z');
  const to = at('2026-11-27T12:00:00Z');
  const plot = { left: 40, top: 0, width: 700, height: 100 };
  const x = linearScale([from, to], [40, 740]);

  it('gives a gap with from = to a visible width', () => {
    const areas = gapAreas([gap('2026-11-24T12:00:00Z', '2026-11-24T12:00:00Z')], from, to, x, plot);
    expect(areas).toHaveLength(1);
    expect(areas[0]?.width).toBe(MIN_GAP_WIDTH);
  });

  it('clamps a gap to the range and keeps it inside the plot', () => {
    const areas = gapAreas([gap('2026-11-19T00:00:00Z', '2026-11-20T12:00:00Z')], from, to, x, plot);
    expect(areas[0]?.x).toBe(40);
    expect(areas[0]?.width).toBe(MIN_GAP_WIDTH);
    const late = gapAreas([gap('2026-11-27T11:59:00Z', '2026-11-27T13:00:00Z')], from, to, x, plot);
    expect((late[0]?.x ?? 0) + (late[0]?.width ?? 0)).toBeLessThanOrEqual(740);
  });

  it('meets an open interval only when it overlaps it', () => {
    const gaps = [gap('2026-11-24T12:00:00Z', '2026-11-24T13:00:00Z')];
    expect(gapBetween(gaps, at('2026-11-24T11:00:00Z'), at('2026-11-24T14:00:00Z'))).toBe(true);
    expect(gapBetween(gaps, at('2026-11-24T13:00:00Z'), at('2026-11-24T14:00:00Z'))).toBe(false);
    expect(gapBetween(gaps, at('2026-11-24T10:00:00Z'), at('2026-11-24T12:00:00Z'))).toBe(false);
  });
});

describe('cursor', () => {
  const points = [point('2026-11-25T10:00:00Z', 1), point('2026-11-25T11:00:00Z', 2), point('2026-11-25T14:00:00Z', 3)];

  it('finds the nearest point', () => {
    expect(nearestIndex(points, at('2026-11-25T09:00:00Z'))).toBe(0);
    expect(nearestIndex(points, at('2026-11-25T10:29:00Z'))).toBe(0);
    expect(nearestIndex(points, at('2026-11-25T10:31:00Z'))).toBe(1);
    expect(nearestIndex(points, at('2026-11-25T13:00:00Z'))).toBe(2);
    expect(nearestIndex(points, at('2026-11-26T00:00:00Z'))).toBe(2);
    expect(nearestIndex([], 0)).toBe(-1);
  });
});

describe('value axis', () => {
  it('uses 0 to 100 for percent', () => {
    expect(valueDomain([40, 60], [0, 100])).toEqual({ domain: [0, 100], ticks: [0, 50, 100] });
  });

  it('uses nice bounds around the values, and a domain around a single value', () => {
    expect(valueDomain([75.31, 75.42], null).domain).toEqual([75.3, 75.45]);
    const single = valueDomain([75.2], null);
    expect(single.domain[0]).toBeLessThan(75.2);
    expect(single.domain[1]).toBeGreaterThan(75.2);
    expect(niceStep(10, 4)).toBe(5);
    expect(niceStep(0, 4)).toBe(1);
  });
});

describe('time axis in Europe/Helsinki', () => {
  it('puts day ticks at local midnight across the end of summer time', () => {
    const midnights = helsinkiMidnights(at('2026-10-23T00:00:00Z'), at('2026-10-28T00:00:00Z'));
    expect(midnights.map((ms) => new Date(ms).toISOString())).toEqual([
      '2026-10-23T21:00:00.000Z',
      '2026-10-24T21:00:00.000Z',
      '2026-10-25T22:00:00.000Z',
      '2026-10-26T22:00:00.000Z',
      '2026-10-27T22:00:00.000Z',
    ]);
  });

  it('puts hour ticks at whole Helsinki hours for 24 h', () => {
    const ticks = timeTicks(at('2026-11-26T12:00:00Z'), at('2026-11-27T12:00:00Z'), 8);
    expect(ticks.length).toBeLessThanOrEqual(8);
    expect(ticks.length).toBeGreaterThanOrEqual(4);
    for (const tick of ticks) {
      const parts = zonedParts(tick.at);
      expect(parts.minute).toBe(0);
      expect(parts.hour % 3).toBe(0);
      expect(tick.kind).toBe(parts.hour === 0 ? 'day' : 'hour');
    }
  });

  it.each([
    ['7d', 168],
    ['30d', 720],
    ['90d', 2_160],
    ['1y', 8_760],
  ])('keeps the ticks of %s at local midnights within the limit', (_key, hours) => {
    const to = at('2026-11-27T12:00:00Z');
    for (const maxTicks of [4, 8]) {
      const ticks = timeTicks(to - hours * 3_600_000, to, maxTicks);
      expect(ticks.length).toBeGreaterThan(0);
      expect(ticks.length).toBeLessThanOrEqual(maxTicks);
      for (const tick of ticks) {
        const parts = zonedParts(tick.at);
        expect([parts.hour, parts.minute]).toEqual([0, 0]);
      }
    }
  });
});
