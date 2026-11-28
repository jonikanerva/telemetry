// The typed view state of the consumption view, with the rules of the
// battery view (battery/view-state.ts): a failed request is never an empty
// result, and earlier data after a failed refresh stays visible with the
// failure.
//
// Display rules: the API sends the distance in miles, as the vehicle sends
// the Odometer; the view shows kilometres. A `null` value is unknown and
// shows as "ei tietoa", never 0.
import type { ConsumptionView, Drive } from 'telemetry-web-api-client';

import { failureReason } from '../battery/view-state.js';
import type { FailureReason } from '../battery/view-state.js';
import { formatClock, formatDateTimeNear, formatDuration, formatNumber } from '../format/format.js';
import { fi } from '../i18n/fi.js';
import type { QuerySnapshot } from '../view-state.js';

/** One mile in kilometres. */
export const MILE_IN_KM = 1.609344;

export type ConsumptionViewState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'unavailable'; readonly reason: FailureReason }
  | { readonly kind: 'stale_usable'; readonly reason: FailureReason; readonly view: ConsumptionView }
  | { readonly kind: 'no_drives'; readonly view: ConsumptionView }
  | { readonly kind: 'data'; readonly view: ConsumptionView };

export function toConsumptionViewState(query: QuerySnapshot<ConsumptionView>): ConsumptionViewState {
  switch (query.status) {
    case 'pending':
      return { kind: 'loading' };
    case 'error': {
      const reason = failureReason(query.error);
      return query.data === undefined
        ? { kind: 'unavailable', reason }
        : { kind: 'stale_usable', reason, view: query.data };
    }
    case 'success':
      if (query.data === undefined) {
        return { kind: 'unavailable', reason: 'invalid_response' };
      }
      return query.data.drives.length === 0
        ? { kind: 'no_drives', view: query.data }
        : { kind: 'data', view: query.data };
    default:
      return unknownStatus(query.status);
  }
}

function unknownStatus(_status: never): ConsumptionViewState {
  return { kind: 'unavailable', reason: 'invalid_response' };
}

/** The text of the one live region: the outcome of the last fetch. */
export function refreshStatus(state: ConsumptionViewState, fetching: boolean): string {
  if (state.kind === 'loading') {
    return fi.consumptionLoading;
  }
  if (fetching) {
    return fi.refreshing;
  }
  switch (state.kind) {
    case 'unavailable':
      return fi.loadFailed;
    case 'stale_usable':
      return fi.refreshFailed(formatClock(Date.parse(state.view.generated_at)));
    case 'no_drives':
    case 'data':
      return fi.fetchedAt(formatClock(Date.parse(state.view.generated_at)));
    default:
      return unknownState(state);
  }
}

function unknownState(_state: never): string {
  return fi.loadFailed;
}

/** Kilometres of a distance in miles, or null. */
export function km(miles: number | null): number | null {
  return miles === null ? null : miles * MILE_IN_KM;
}

/** A value with `digits` decimals, or "ei tietoa". */
export function valueText(value: number | null, digits: number): string {
  return value === null ? fi.unknownValue : formatNumber(value, digits);
}

/** The cells of one row of the drive table, in the order of the columns. */
export function driveCells(drive: Drive, generatedAtMs: number): string[] {
  const start = formatDateTimeNear(Date.parse(drive.start), generatedAtMs);
  if (drive.end === null) {
    return [start, fi.driveOngoing, fi.notYet, fi.notYet, fi.notYet, fi.notYet, fi.notYet];
  }
  return [
    start,
    drive.duration_seconds === null ? fi.unknownValue : formatDuration(drive.duration_seconds * 1000),
    valueText(km(drive.distance), 1),
    valueText(drive.energy_used, 2),
    valueText(drive.consumption, 0),
    valueText(drive.mean_outside_temp, 1),
    valueText(drive.mean_speed, 0),
  ];
}

/** The sentences of the totals of the range. */
export function totalsText(view: ConsumptionView): string[] {
  const totals = view.totals;
  const sentences: string[] = [];
  if (totals.consumption !== null) {
    sentences.push(
      fi.totalsLine(
        totals.drives - totals.drives_without_consumption,
        valueText(km(totals.distance), 1),
        valueText(totals.energy_used, 1),
        valueText(totals.consumption, 0),
      ),
    );
  }
  if (totals.drives_without_consumption > 0) {
    sentences.push(fi.totalsUnknown(totals.drives_without_consumption));
  }
  if (view.drives.some((drive) => drive.end === null)) {
    sentences.push(fi.totalsOngoing);
  }
  if (view.truncated) {
    sentences.push(fi.consumptionTruncated);
  }
  return sentences;
}

/** True when an ended drive has an unknown value. */
export function hasUnknownValues(view: ConsumptionView): boolean {
  return view.totals.drives_without_consumption > 0;
}
