import { describe, expect, it } from 'vitest';

import { point, series } from '../battery/fixtures.js';
import { chartSummary } from './TimeSeriesChart.js';

const generatedAtMs = Date.parse('2026-11-27T12:00:00Z');
const soc = series([[point('2026-11-27T11:50:00Z', 61)]]);
const points = soc.segments.flat();

describe('the gap count of the chart summary', () => {
  it('states the count of a complete list', () => {
    expect(chartSummary(soc, points, { count: 2, truncated: false }, soc.unit, generatedAtMs)).toContain(
      '2 aukkoa valitulla välillä.',
    );
  });

  it('states only a lower bound when the list is truncated', () => {
    const sentences = chartSummary(soc, points, { count: 500, truncated: true }, soc.unit, generatedAtMs);
    expect(sentences).toContain('Vähintään 500 aukkoa valitulla välillä.');
    expect(sentences).not.toContain('500 aukkoa valitulla välillä.');
  });
});
