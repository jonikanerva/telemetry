import { ApiRequestError } from 'telemetry-web-api-client';
import type { ConsumptionView, Drive } from 'telemetry-web-api-client';
import { describe, expect, it } from 'vitest';

import type { QuerySnapshot } from '../view-state.js';
import {
  consumptionStatusLine,
  lossesLine,
  noDrivesText,
  scatterSummary,
  driveCells,
  hasUnknownValues,
  km,
  refreshStatus,
  toConsumptionViewState,
  totalsText,
} from './view-state.js';

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
  data_loss: false,
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
  data_loss: false,
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
  data_loss: false,
};

function view(drives: Drive[], overrides: Partial<ConsumptionView> = {}): ConsumptionView {
  return {
    generated_at: '2026-11-27T12:00:00Z',
    range: { key: '7d', from: '2026-11-20T12:00:00Z', to: '2026-11-27T12:00:00Z' },
    status: {
      connection: { state: 'connected', at: '2026-11-27T11:00:00Z' },
      latest_data_at: '2026-11-27T11:50:00Z',
      latest_received_at: '2026-11-27T11:50:02Z',
      freshness: 'current',
    },
    gaps: { items: [], truncated: false },
    units: { distance: 'mi', energy: 'kWh', consumption: 'Wh/km', temperature: '°C', speed: 'km/h' },
    drives,
    totals: {
      drives: 1,
      distance: 12.5,
      energy_used: 4,
      consumption: 198.838,
      drives_without_consumption: 0,
      drives_with_data_loss: 0,
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
      '4,0',
      '199',
      '−1,3',
      '40',
    ]);
  });

  it('shows an unknown value as unknown, never 0', () => {
    const cells = driveCells(unknown, Date.parse('2026-11-27T12:00:00Z'));
    expect(cells.slice(2)).toEqual(['ei tietoa', '4,0', 'ei tietoa', 'ei tietoa', 'ei tietoa']);
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
      totals: {
        drives: 2,
        distance: 12.5,
        energy_used: 4,
        consumption: 198.838,
        drives_without_consumption: 1,
        drives_with_data_loss: 0,
      },
      truncated: true,
    });
    expect(totalsText(body)).toEqual([
      '1 ajo: 20,1 km, 4,0 kWh, 199 Wh/km.',
      '1 ajolla ei ole kulutusarvoa (matka tai energia puuttuu, tai matka on alle 1 km), eikä se ole mukana summissa.',
      'Käynnissä oleva ajo ei ole mukana summissa.',
      'Aikavälillä on enemmän dataa kuin yksi haku lukee: vanhimmat ajot puuttuvat.',
    ]);
  });
});

describe('the states of the battery view', () => {
  const asleep = view([ended], {
    status: {
      connection: { state: 'disconnected', at: '2026-11-27T10:40:00Z' },
      latest_data_at: '2026-11-27T10:35:00Z',
      latest_received_at: '2026-11-27T10:35:02Z',
      freshness: 'not_expected',
    },
  });

  it('shows the asleep state with the status line of the battery view', () => {
    expect(consumptionStatusLine(asleep)).toBe(
      'Ei yhteyttä tänään klo 12.40 alkaen: auto nukkuu tai on verkon ulkopuolella. Uusin data tänään klo 12.35.',
    );
    expect(refreshStatus({ kind: 'data', view: asleep }, false)).toBe('Haettu klo 14.00');
  });

  it('shows stale data in the status line and in the live region', () => {
    const stale = view([ended], {
      status: {
        connection: { state: 'connected', at: '2026-11-27T10:00:00Z' },
        latest_data_at: '2026-11-27T10:35:00Z',
        latest_received_at: '2026-11-27T10:35:02Z',
        freshness: 'overdue',
      },
    });
    expect(consumptionStatusLine(stale)).toBe(
      'Yhdistetty tänään klo 12.00, mutta uutta dataa ei ole tallentunut yli 20 minuuttiin. Uusin data tänään klo 12.35.',
    );
    expect(refreshStatus({ kind: 'data', view: stale }, false)).toBe('Haettu klo 14.00. Data on myöhässä.');
  });

  it('marks a drive with a recorded loss and keeps its values', () => {
    const marked: Drive = { ...ended, data_loss: true };
    const body = view([marked], {
      gaps: {
        items: [
          {
            kind: 'queue_full',
            from: '2026-11-27T10:10:00Z',
            to: '2026-11-27T10:11:00Z',
            count: { value: 3, of: 'records' },
          },
        ],
        truncated: true,
      },
      totals: { ...view([]).totals, drives_with_data_loss: 1 },
    });
    const cells = driveCells(marked, Date.parse('2026-11-27T12:00:00Z'));
    expect(cells[0]).toBe('tänään klo 12.00 (datan menetys ajon aikana)');
    expect(cells[2]).toBe('20,1');
    expect(totalsText(body)).toContain(
      '1 ajo on merkitty: datan menetys ajon aikana, joten sen arvot voivat olla väärin.',
    );
    expect(totalsText(body)).toContain('Välillä on enemmän menetyksiä kuin luettelo näyttää.');
    expect(hasUnknownValues(body)).toBe(true);
  });

  it('says when no battery row exists yet', () => {
    expect(consumptionStatusLine(view([], { status: null }))).toBe('Autosta ei ole vielä tallentunut akun tietoja.');
  });

  it('counts a drive without a temperature as partial data', () => {
    expect(hasUnknownValues(view([ended]))).toBe(false);
    expect(hasUnknownValues(view([{ ...ended, mean_outside_temp: null }]))).toBe(true);
  });
});

describe('a cut read and the recorded losses', () => {
  const loss = {
    kind: 'queue_full',
    from: '2026-11-27T10:10:00Z',
    to: '2026-11-27T10:11:00Z',
    count: { value: 3, of: 'records' },
  } as const;

  it('never says "no drives" as a fact when the read was cut', () => {
    expect(noDrivesText(view([]))).toBe('Ei ajoja valitulla aikavälillä.');
    expect(noDrivesText(view([], { truncated: true }))).toBe(
      'Aikavälillä on enemmän dataa kuin yksi haku lukee: vanhimmat ajot puuttuvat.',
    );
  });

  it('shows the recorded losses of the range, also a loss that meets no drive', () => {
    expect(lossesLine(view([ended]))).toBeNull();
    expect(lossesLine(view([], { gaps: { items: [loss], truncated: false } }))).toBe('1 aukko valitulla välillä.');
    expect(lossesLine(view([], { gaps: { items: [loss, loss], truncated: true } }))).toBe(
      'Vähintään 2 aukkoa valitulla välillä.',
    );
  });

  it('tells how many plotted drives have a loss mark', () => {
    expect(scatterSummary(3, 4, 0)).toEqual(['3/4 ajoa kaaviossa.']);
    expect(scatterSummary(3, 4, 1)).toEqual([
      '3/4 ajoa kaaviossa.',
      '1 kaavion ajoista on merkitty: datan menetys ajon aikana.',
    ]);
  });
});
