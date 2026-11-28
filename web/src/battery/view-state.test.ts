import { ApiRequestError } from 'telemetry-web-api-client';
import type { ApiError, BatteryView, ProblemCode } from 'telemetry-web-api-client';
import { describe, expect, it } from 'vitest';

import { availableView, noDataYetView, point, series } from './fixtures.js';
import { seriesLines } from '../chart/geometry.js';
import { apiFailureReason, refreshStatus, statusLine, toBatteryViewState } from './view-state.js';
import type { NoDataYetView } from './view-state.js';
import type { QuerySnapshot } from '../view-state.js';

function snapshot(
  status: QuerySnapshot<BatteryView>['status'],
  data?: BatteryView,
  error: unknown = null,
): QuerySnapshot<BatteryView> {
  return { status, data, error };
}

function problem(code: ProblemCode, status: number): ApiError {
  return { kind: 'problem', problem: { type: 'about:blank', title: 'x', status, code, detail: 'x' } };
}

describe('toBatteryViewState', () => {
  it('is loading while the first request runs', () => {
    expect(toBatteryViewState(snapshot('pending'))).toEqual({ kind: 'loading' });
  });

  it('is no_data_yet and data only for a valid success body', () => {
    expect(toBatteryViewState(snapshot('success', noDataYetView))).toEqual({
      kind: 'no_data_yet',
      view: noDataYetView,
    });
    const view = availableView();
    expect(toBatteryViewState(snapshot('success', view))).toEqual({ kind: 'data', view });
  });

  it('never turns a failure into an empty result', () => {
    const error = new ApiRequestError(problem('database_unavailable', 503));
    expect(toBatteryViewState(snapshot('error', undefined, error))).toEqual({
      kind: 'unavailable',
      reason: 'database_unavailable',
    });
  });

  it('keeps the earlier data visible when a refresh fails', () => {
    const view = availableView();
    const error = new ApiRequestError({ kind: 'network' });
    expect(toBatteryViewState(snapshot('error', view, error))).toEqual({
      kind: 'stale_usable',
      reason: 'network',
      view,
    });
  });

  it('asks for a reload when the response fails validation', () => {
    const error = new ApiRequestError({ kind: 'invalid_response', status: 200 });
    expect(toBatteryViewState(snapshot('error', undefined, error))).toEqual({
      kind: 'unavailable',
      reason: 'invalid_response',
    });
  });

  it('treats an unknown thrown value and a success without data as invalid', () => {
    expect(toBatteryViewState(snapshot('error', undefined, new Error('boom')))).toEqual({
      kind: 'unavailable',
      reason: 'invalid_response',
    });
    expect(toBatteryViewState(snapshot('success'))).toEqual({ kind: 'unavailable', reason: 'invalid_response' });
  });
});

describe('apiFailureReason', () => {
  it.each([
    [problem('database_unavailable', 503), 'database_unavailable'],
    [problem('service_busy', 503), 'service_busy'],
    [problem('timeout', 503), 'timeout'],
    [problem('internal_error', 500), 'server_error'],
    [problem('data_contract_violation', 500), 'data_contract_violation'],
    [problem('unauthenticated', 401), 'unauthenticated'],
    [problem('invalid_request', 400), 'rejected_request'],
    [problem('forbidden_origin', 403), 'rejected_request'],
    [{ kind: 'timeout' } as const, 'timeout'],
    [{ kind: 'network' } as const, 'network'],
    [{ kind: 'invalid_response', status: 502 } as const, 'invalid_response'],
  ])('maps %o to %s', (error, reason) => {
    expect(apiFailureReason(error)).toBe(reason);
  });
});

// The seven states of VISION.md on the view model: loading, unavailable,
// and no data yet above; data, gap, asleep, and stale below. The texts
// are written by hand.
describe('the status line: one sentence for the connection fact and the freshness', () => {
  const base = availableView();

  it('data: connected with current data', () => {
    expect(toBatteryViewState(snapshot('success', base))).toMatchObject({ kind: 'data' });
    expect(statusLine(base)).toBe('Yhdistetty tänään klo 13.00. Uusin data tänään klo 13.50.');
  });

  it('stale: connected, but no new data arrives', () => {
    const view = availableView({ status: { ...base.status, freshness: 'overdue' } });
    expect(statusLine(view)).toBe(
      'Yhdistetty tänään klo 13.00, mutta uutta dataa ei ole tallentunut yli 20 minuuttiin. Uusin data tänään klo 13.50.',
    );
  });

  it('asleep: disconnected since a time, one state for asleep and offline', () => {
    const view = availableView({
      status: {
        connection: { state: 'disconnected', at: '2026-11-25T12:01:00Z' },
        latest_data_at: '2026-11-25T12:00:00Z',
        latest_received_at: '2026-11-25T12:00:02Z',
        freshness: 'not_expected',
      },
    });
    expect(statusLine(view)).toBe(
      'Ei yhteyttä 25.11.2026 klo 14.01 alkaen: auto nukkuu tai on verkon ulkopuolella. Uusin data 25.11.2026 klo 14.00.',
    );
  });

  it('asleep without any stored connection event', () => {
    const view = availableView({
      status: { ...base.status, connection: { state: 'none' }, freshness: 'not_expected' },
    });
    expect(statusLine(view)).toBe(
      'Ei yhteystietoja: auto nukkuu tai on verkon ulkopuolella. Uusin data tänään klo 13.50.',
    );
  });

  it('gap: the recorded losses split the line of the chart', () => {
    const gap = {
      kind: 'queue_full',
      from: '2026-11-26T10:30:00Z',
      to: '2026-11-26T10:40:00Z',
      count: { value: 3, of: 'records' },
    } as const;
    const view = availableView({
      gaps: { items: [gap], truncated: false },
      charts: {
        ...base.charts,
        soc: series([[point('2026-11-26T10:00:00Z', 70)], [point('2026-11-26T11:00:00Z', 69)]]),
      },
    });
    expect(toBatteryViewState(snapshot('success', view))).toMatchObject({ kind: 'data' });
    expect(seriesLines(view.charts.soc, view.gaps.items)).toHaveLength(2);
  });
});

describe('refreshStatus: the text of the one live region', () => {
  const view = availableView();

  it('tells the request while it runs, also for the first load', () => {
    expect(refreshStatus({ kind: 'loading' }, true)).toBe('Haetaan akun tietoja…');
    expect(refreshStatus({ kind: 'data', view }, true)).toBe('Päivitetään…');
    expect(refreshStatus({ kind: 'stale_usable', reason: 'network', view }, true)).toBe('Päivitetään…');
  });

  it('tells new data with its fetch time, and late data as late', () => {
    expect(refreshStatus({ kind: 'data', view }, false)).toBe('Haettu klo 14.00');
    const overdue = availableView({ status: { ...view.status, freshness: 'overdue' } });
    expect(refreshStatus({ kind: 'data', view: overdue }, false)).toBe('Haettu klo 14.00. Data on myöhässä.');
    expect(refreshStatus({ kind: 'no_data_yet', view: noDataYetView as NoDataYetView }, false)).toBe(
      'Haettu klo 14.00',
    );
  });

  it('tells a failed refresh as a failure, with the time of the data that stays', () => {
    expect(refreshStatus({ kind: 'stale_usable', reason: 'server_error', view }, false)).toBe(
      'Päivitys epäonnistui. Näytetään aiemmin haettu data (haettu klo 14.00).',
    );
  });

  it('tells a failed first load as a failure, never as data', () => {
    expect(refreshStatus({ kind: 'unavailable', reason: 'timeout' }, false)).toBe('Tietoja ei saatu.');
  });
});
