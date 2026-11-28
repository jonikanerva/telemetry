import { ApiRequestError } from 'telemetry-web-api-client';
import type { ConsumptionView, Drive } from 'telemetry-web-api-client';
import { describe, expect, it } from 'vitest';

import type { QuerySnapshot } from '../view-state.js';
import { driveCells, km, refreshStatus, toConsumptionViewState, totalsText } from './view-state.js';

// Synthetic values. 12.5 mi = 20.1168 km; 4 kWh / 20.1168 km = 198.8 Wh/km.
const ended: Drive = {
  start: '2026-11-27T10:00:00Z',
  end: '2026-11-27T10:30:00Z',
  duration_seconds: 1800,
  distance: 12.5,
  energy_used: 4,
  consumption: 198.838,
  mean_outside_temp: -1.25,
  mean_speed: 40.2336,
};

const unknown: Drive = {
  ...ended,
  start: '2026-11-27T08:00:00Z',
  end: '2026-11-27T08:10:00Z',
  duration_seconds: 600,
  distance: null,
  consumption: null,
  mean_outside_temp: null,
  mean_speed: null,
};

const ongoing: Drive = {
  start: '2026-11-27T11:50:00Z',
  end: null,
  duration_seconds: null,
  distance: null,
  energy_used: null,
  consumption: null,
  mean_outside_temp: null,
  mean_speed: null,
};

function view(drives: Drive[], overrides: Partial<ConsumptionView> = {}): ConsumptionView {
  return {
    generated_at: '2026-11-27T12:00:00Z',
    range: { key: '7d', from: '2026-11-20T12:00:00Z', to: '2026-11-27T12:00:00Z' },
    units: { distance: 'mi', energy: 'kWh', consumption: 'Wh/km', temperature: '°C', speed: 'km/h' },
    drives,
    totals: {
      drives: 1,
      distance: 12.5,
      energy_used: 4,
      consumption: 198.838,
      drives_without_consumption: 0,
    },
    truncated: false,
    ...overrides,
  };
}

function snapshot(
  status: QuerySnapshot<ConsumptionView>['status'],
  data?: ConsumptionView,
  error: unknown = null,
): QuerySnapshot<ConsumptionView> {
  return { status, data, error };
}

describe('toConsumptionViewState', () => {
  it('is loading while the first request runs', () => {
    const state = toConsumptionViewState(snapshot('pending'));
    expect(state).toEqual({ kind: 'loading' });
    expect(refreshStatus(state, true)).toBe('Haetaan ajoja…');
  });

  it('is no_drives for a valid body without drives', () => {
    const body = view([], { totals: { ...view([]).totals, drives: 0, distance: null } });
    const state = toConsumptionViewState(snapshot('success', body));
    expect(state).toEqual({ kind: 'no_drives', view: body });
    expect(refreshStatus(state, false)).toBe('Haettu klo 14.00');
  });

  it('is data for a valid body with drives', () => {
    const body = view([ended]);
    expect(toConsumptionViewState(snapshot('success', body))).toEqual({ kind: 'data', view: body });
  });

  it('never turns a failure into an empty result', () => {
    const error = new ApiRequestError({
      kind: 'problem',
      problem: { type: 'about:blank', title: 'x', status: 503, code: 'database_unavailable', detail: 'x' },
    });
    const state = toConsumptionViewState(snapshot('error', undefined, error));
    expect(state).toEqual({ kind: 'unavailable', reason: 'database_unavailable' });
    expect(refreshStatus(state, false)).toBe('Tietoja ei saatu.');
  });

  it('keeps the earlier data, marked as old, after a failed refresh', () => {
    const body = view([ended]);
    const state = toConsumptionViewState(snapshot('error', body, new ApiRequestError({ kind: 'network' })));
    expect(state).toEqual({ kind: 'stale_usable', reason: 'network', view: body });
    expect(refreshStatus(state, false)).toBe('Päivitys epäonnistui. Näytetään aiemmin haettu data (haettu klo 14.00).');
  });
});

describe('display values', () => {
  it('converts miles to kilometres for display only', () => {
    expect(km(12.5)).toBeCloseTo(20.1168, 10);
    expect(km(null)).toBeNull();
  });

  it('shows the values of an ended drive in Finnish formats', () => {
    expect(driveCells(ended, Date.parse('2026-11-27T12:00:00Z'))).toEqual([
      'tänään klo 12.00',
      '30\u00a0min',
      '20,1',
      '4',
      '199',
      '−1,3',
      '40',
    ]);
  });

  it('shows an unknown value as unknown, never 0', () => {
    const cells = driveCells(unknown, Date.parse('2026-11-27T12:00:00Z'));
    expect(cells.slice(2)).toEqual(['ei tietoa', '4', 'ei tietoa', 'ei tietoa', 'ei tietoa']);
  });

  it('shows an ongoing drive without values', () => {
    expect(driveCells(ongoing, Date.parse('2026-11-27T12:00:00Z')).slice(1)).toEqual([
      'käynnissä',
      '–',
      '–',
      '–',
      '–',
      '–',
    ]);
  });

  it('tells the partial data, the ongoing drive, and the cut in the totals', () => {
    const body = view([ongoing, ended, unknown], {
      totals: { drives: 2, distance: 12.5, energy_used: 4, consumption: 198.838, drives_without_consumption: 1 },
      truncated: true,
    });
    expect(totalsText(body)).toEqual([
      '1 ajo: 20,1 km, 4 kWh, 199 Wh/km.',
      '1 ajolla ei ole kulutusarvoa (matka tai energia puuttuu, tai matka on alle 1 km), eikä se ole mukana summissa.',
      'Käynnissä oleva ajo ei ole mukana summissa.',
      'Aikavälillä on enemmän dataa kuin yksi haku lukee: vanhimmat ajot puuttuvat.',
    ]);
  });
});
