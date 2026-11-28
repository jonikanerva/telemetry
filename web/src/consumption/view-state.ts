// The typed view state of the consumption view, with the rules of the
// battery view (battery/view-state.ts): a failed request is never an empty
// result, and earlier data after a failed refresh stays visible with the
// failure.
//
// Display rules: the API sends the distance in miles, as the vehicle sends
// the Odometer; the view shows kilometres. A `null` value is unknown and
// shows as "ei tietoa", never 0.
import type { ConsumptionView, Drive } from 'telemetry-web-api-client';

import { failureReason, statusText } from '../battery/view-state.js';
import type { FailureReason } from '../battery/view-state.js';
import { formatClock, formatDateTimeNear, formatDuration, formatFixed } from '../format/format.js';
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
    case 'data': {
      // The freshness of the battery view: stale data is announced.
      const clock = formatClock(Date.parse(state.view.generated_at));
      return state.view.status?.freshness === 'overdue' ? fi.fetchedAtOverdue(clock) : fi.fetchedAt(clock);
    }
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

/** A value with exactly `digits` decimals, or "ei tietoa". */
export function valueText(value: number | null, digits: number): string {
  return value === null ? fi.unknownValue : formatFixed(value, digits);
}

/**
 * The status line of the battery view (connection, newest data, and
 * freshness: asleep is `not_expected`, stale is `overdue`), or a text
 * without a status when no battery row exists yet.
 */
export function consumptionStatusLine(view: ConsumptionView): string {
  return view.status === null ? fi.noStatusYet : statusText(view.status, view.generated_at);
}

/** The cells of one row of the drive table, in the order of the columns. */
export function driveCells(drive: Drive, generatedAtMs: number): string[] {
  const when = formatDateTimeNear(Date.parse(drive.start), generatedAtMs);
  const start = drive.data_loss ? `${when} (${fi.driveDataLoss})` : when;
  if (drive.end === null) {
    return [start, fi.driveOngoing, fi.notYet, fi.notYet, fi.notYet, fi.notYet, fi.notYet];
  }
  return [
    start,
    drive.duration_seconds === null ? fi.unknownValue : formatDuration(drive.duration_seconds * 1000),
    valueText(km(drive.distance), 1),
    valueText(drive.energy_used, 1),
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
  if (totals.drives_with_data_loss > 0) {
    sentences.push(fi.totalsDataLoss(totals.drives_with_data_loss));
  }
  if (view.gaps.truncated) {
    sentences.push(fi.gapsTruncated);
  }
  if (view.truncated) {
    sentences.push(fi.consumptionTruncated);
  }
  return sentences;
}

/** True when an ended drive has an unknown value in any column, or a loss mark. */
export function hasUnknownValues(view: ConsumptionView): boolean {
  return view.drives.some(
    (drive) =>
      drive.end !== null &&
      (drive.data_loss ||
        [drive.distance, drive.energy_used, drive.consumption, drive.mean_outside_temp, drive.mean_speed].includes(
          null,
        )),
  );
}
