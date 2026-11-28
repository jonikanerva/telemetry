// The typed view state of the battery view (design §7, profile §2.4).
//
// Rules:
// - A failed request is never an empty result. Without earlier data it is
//   `unavailable`; with earlier data of the same range it is
//   `stale_usable`: the earlier data stays visible, with the failure.
// - A response that fails validation is `unavailable` with the reason
//   `invalid_response`, which asks for a page reload. A 400 or 403
//   (`rejected_request`) also asks for a reload.
// - A 401 is `unauthenticated`; the session-loss handler of the query
//   client then opens the login page (auth/session-loss.ts).
// - `no_data_yet` and `data` come only from a valid success body.
import type { ApiError, BatteryView, ProblemCode } from 'telemetry-web-api-client';
import { ApiRequestError } from 'telemetry-web-api-client';

import { formatClock, formatDateTimeNear } from '../format/format.js';
import { fi } from '../i18n/fi.js';
import type { QuerySnapshot } from '../view-state.js';

/** Why the data is not available. Each reason has its own text. */
export type FailureReason =
  | 'database_unavailable'
  | 'service_busy'
  | 'timeout'
  | 'server_error'
  | 'data_contract_violation'
  | 'unauthenticated'
  | 'invalid_response'
  | 'rejected_request'
  | 'network';

export type AvailableView = Extract<BatteryView, { state: 'available' }>;
export type NoDataYetView = Extract<BatteryView, { state: 'no_data_yet' }>;

export type BatteryViewState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'unavailable'; readonly reason: FailureReason }
  | { readonly kind: 'stale_usable'; readonly reason: FailureReason; readonly view: BatteryView }
  | { readonly kind: 'no_data_yet'; readonly view: NoDataYetView }
  | { readonly kind: 'data'; readonly view: AvailableView };

export function toBatteryViewState(query: QuerySnapshot<BatteryView>): BatteryViewState {
  switch (query.status) {
    case 'pending':
      return { kind: 'loading' };
    case 'error': {
      const reason = failureReason(query.error);
      // TanStack Query keeps the data of the last success after a failed
      // refetch of the same key.
      return query.data === undefined
        ? { kind: 'unavailable', reason }
        : { kind: 'stale_usable', reason, view: query.data };
    }
    case 'success':
      if (query.data === undefined) {
        return { kind: 'unavailable', reason: 'invalid_response' };
      }
      return query.data.state === 'no_data_yet'
        ? { kind: 'no_data_yet', view: query.data }
        : { kind: 'data', view: query.data };
    default:
      return unknownStatus(query.status);
  }
}

function unknownStatus(_status: never): BatteryViewState {
  return { kind: 'unavailable', reason: 'invalid_response' };
}

/** The reason of a failed query. An error of another type is a client fault. */
export function failureReason(error: unknown): FailureReason {
  if (!(error instanceof ApiRequestError)) {
    return 'invalid_response';
  }
  return apiFailureReason(error.error);
}

export function apiFailureReason(error: ApiError): FailureReason {
  switch (error.kind) {
    case 'network':
      return 'network';
    case 'timeout':
      return 'timeout';
    case 'invalid_response':
      return 'invalid_response';
    case 'problem':
      return problemReason(error.problem.code);
    default:
      return unknownError(error);
  }
}

function unknownError(_error: never): FailureReason {
  return 'invalid_response';
}

function problemReason(code: ProblemCode): FailureReason {
  switch (code) {
    case 'database_unavailable':
      return 'database_unavailable';
    case 'service_busy':
      return 'service_busy';
    case 'timeout':
      return 'timeout';
    case 'data_contract_violation':
      return 'data_contract_violation';
    case 'unauthenticated':
      return 'unauthenticated';
    // 400 (an invalid query, or two session cookies) and 403 (a refused
    // origin): a reload sends a clean request, and the session check of
    // the reload explains two session cookies (auth/conflicting-cookies.ts).
    case 'invalid_request':
    case 'forbidden_origin':
      return 'rejected_request';
    case 'internal_error':
    case 'invalid_credentials':
    case 'login_throttled':
    case 'unsupported_media_type':
    case 'payload_too_large':
    case 'not_found':
    case 'method_not_allowed':
      return 'server_error';
    default:
      return unknownCode(code);
  }
}

function unknownCode(_code: never): FailureReason {
  return 'server_error';
}

/**
 * The one status line of the view: the stored connection fact, its time,
 * and the freshness of the data (ADR 0013). The data cannot tell a
 * sleeping car from a car without network, so both are one state.
 */
export function statusLine(view: AvailableView): string {
  const generatedAtMs = Date.parse(view.generated_at);
  const connection = view.status.connection;
  let fact: string;
  switch (connection.state) {
    case 'connected':
      fact = fi.connectionConnected(formatDateTimeNear(Date.parse(connection.at), generatedAtMs));
      break;
    case 'disconnected':
      fact = fi.connectionDisconnected(formatDateTimeNear(Date.parse(connection.at), generatedAtMs));
      break;
    case 'none':
      fact = fi.connectionNone;
      break;
    default:
      fact = unknownConnection(connection);
  }
  return `${fact}${freshnessText(view.status.freshness)} ${fi.latestData(formatDateTimeNear(Date.parse(view.status.latest_data_at), generatedAtMs))}`;
}

function freshnessText(freshness: AvailableView['status']['freshness']): string {
  switch (freshness) {
    case 'current':
      return '.';
    case 'overdue':
      return fi.freshnessOverdue;
    case 'not_expected':
      return fi.freshnessNotExpected;
    default:
      return unknownFreshness(freshness);
  }
}

function unknownConnection(_connection: never): string {
  return fi.connectionNone;
}

function unknownFreshness(_freshness: never): string {
  return '.';
}

/**
 * The text of the one live region of the view (WCAG 2.2 AA 4.1.3). It
 * tells the outcome of the last fetch, once and truthfully:
 *
 * - a request runs (the first load or a refresh): the request;
 * - new data: its fetch time, and "data on myöhässä" when the data is
 *   overdue;
 * - a failed refresh: the failure, and the time of the earlier data that
 *   the view keeps;
 * - a failed first load: the failure.
 *
 * The state banners of the view are not live regions, so no outcome is
 * announced twice. The times come from `generated_at` of the server.
 */
export function refreshStatus(state: BatteryViewState, fetching: boolean): string {
  if (state.kind === 'loading') {
    return fi.batteryLoading;
  }
  if (fetching) {
    return fi.refreshing;
  }
  switch (state.kind) {
    case 'unavailable':
      return fi.loadFailed;
    case 'stale_usable':
      return fi.refreshFailed(formatClock(Date.parse(state.view.generated_at)));
    case 'no_data_yet':
      return fi.fetchedAt(formatClock(Date.parse(state.view.generated_at)));
    case 'data': {
      const clock = formatClock(Date.parse(state.view.generated_at));
      return state.view.status.freshness === 'overdue' ? fi.fetchedAtOverdue(clock) : fi.fetchedAt(clock);
    }
    default:
      return unknownState(state);
  }
}

function unknownState(_state: never): string {
  return fi.loadFailed;
}
