// Synthetic wire values for the unit tests. No real data.
import type { BatteryView, Point, Series } from 'telemetry-web-api-client';

import type { AvailableView } from './view-state.js';

export const percent = { symbol: 'percent', source: 'documented' } as const;
export const kwh = { symbol: 'kilowatt_hour', source: 'documented' } as const;

export function point(at: string, last: number, extra: Partial<Point> = {}): Point {
  return { at, last, min: last, max: last, count: 1, ...extra };
}

export function series(segments: Point[][], carryIn: Point | null = null): Series {
  return { unit: percent, carry_in: carryIn, segments, excluded: { invalid_marker: 0, not_numeric: 0 } };
}

export function availableView(overrides: Partial<AvailableView> = {}): AvailableView {
  return {
    state: 'available',
    generated_at: '2026-11-27T12:00:00Z',
    range: { key: '7d', from: '2026-11-20T12:00:00Z', to: '2026-11-27T12:00:00Z', bucket_seconds: 600 },
    status: {
      connection: { state: 'connected', at: '2026-11-27T11:00:00Z' },
      latest_data_at: '2026-11-27T11:50:00Z',
      latest_received_at: '2026-11-27T11:50:02Z',
      freshness: 'current',
    },
    tiles: {
      soc: { state: 'value', value: 78, unit: percent, measured_at: '2026-11-27T11:50:00Z', newer_unusable_at: null },
      energy_remaining: { state: 'missing', unit: kwh, latest_unusable_at: null },
      outside_temp: {
        state: 'missing',
        unit: { symbol: 'degree_celsius', source: 'assumed' },
        latest_unusable_at: null,
      },
      nominal_full_pack_energy: {
        reading: { state: 'missing', unit: kwh, latest_unusable_at: null },
        change: { state: 'unknown', reason: 'no_latest' },
      },
    },
    charts: { soc: series([[point('2026-11-27T11:50:00Z', 78)]]), nominal_full_pack_energy: series([]) },
    gaps: { items: [], truncated: false },
    restarts: { items: [], truncated: false },
    ...overrides,
  };
}

export const noDataYetView: BatteryView = {
  state: 'no_data_yet',
  generated_at: '2026-11-27T12:00:00Z',
  range: { key: '7d', from: '2026-11-20T12:00:00Z', to: '2026-11-27T12:00:00Z', bucket_seconds: 600 },
};
