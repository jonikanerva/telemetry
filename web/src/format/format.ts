// Display formats of numbers, units, instants, and durations.
//
// Rules:
// - The wire carries UTC instants. The display zone is always
//   Europe/Helsinki, never the zone of the browser or the host.
// - Numbers use the Finnish format (decimal comma, U+2212 minus sign).
// - Units are converted for display only; the value stays as the vehicle
//   sent it. The display rounds to a fixed number of decimals.
// - Relative ages come from two server instants, never from the clock
//   of the browser.
import type { Unit, UnitSymbol } from 'telemetry-web-api-client';

import { fi } from '../i18n/fi.js';

export const DISPLAY_TIME_ZONE = 'Europe/Helsinki';
const LOCALE = 'fi-FI';

const numberFormats = new Map<number, Intl.NumberFormat>();

function numberFormat(fractionDigits: number): Intl.NumberFormat {
  let format = numberFormats.get(fractionDigits);
  if (format === undefined) {
    format = new Intl.NumberFormat(LOCALE, {
      minimumFractionDigits: 0,
      maximumFractionDigits: fractionDigits,
    });
    numberFormats.set(fractionDigits, format);
  }
  return format;
}

/** A number in the Finnish format with at most `fractionDigits` decimals. */
export function formatNumber(value: number, fractionDigits = 1): string {
  return numberFormat(fractionDigits).format(value);
}

/** A number in the Finnish format with exactly `fractionDigits` decimals, for a table column. */
export function formatFixed(value: number, fractionDigits: number): string {
  return new Intl.NumberFormat(LOCALE, {
    minimumFractionDigits: fractionDigits,
    maximumFractionDigits: fractionDigits,
  }).format(value);
}

/** A signed number: a plus sign for a positive value, U+2212 for a negative one. */
export function formatSigned(value: number, fractionDigits = 1): string {
  const text = formatNumber(Math.abs(value), fractionDigits);
  if (text === formatNumber(0, fractionDigits)) {
    return text;
  }
  return value > 0 ? `+${text}` : `−${text}`;
}

export function unitSymbol(symbol: UnitSymbol): string {
  switch (symbol) {
    case 'percent':
      return '%';
    case 'kilowatt_hour':
      return 'kWh';
    case 'degree_celsius':
      return '°C';
    default:
      return unknownSymbol(symbol);
  }
}

function unknownSymbol(_symbol: never): string {
  return '';
}

/** A value with its unit, for example "80,5 %" (a no-break space between). */
export function formatValue(value: number, unit: Unit, fractionDigits = 1): string {
  return `${formatNumber(value, fractionDigits)} ${unitSymbol(unit.symbol)}`;
}

/** The parts of an instant in the display zone. */
export type ZonedParts = {
  readonly year: number;
  readonly month: number;
  readonly day: number;
  readonly hour: number;
  readonly minute: number;
};

const partsFormat = new Intl.DateTimeFormat('en-GB', {
  timeZone: DISPLAY_TIME_ZONE,
  year: 'numeric',
  month: 'numeric',
  day: 'numeric',
  hour: 'numeric',
  minute: 'numeric',
  hourCycle: 'h23',
});

/** The calendar parts of an instant in Europe/Helsinki. */
export function zonedParts(epochMs: number): ZonedParts {
  const values: Record<string, number> = {};
  for (const part of partsFormat.formatToParts(epochMs)) {
    if (part.type !== 'literal') {
      values[part.type] = Number.parseInt(part.value, 10);
    }
  }
  return {
    year: values['year'] ?? 0,
    month: values['month'] ?? 0,
    day: values['day'] ?? 0,
    hour: values['hour'] ?? 0,
    minute: values['minute'] ?? 0,
  };
}

function pad(value: number): string {
  return value < 10 ? `0${String(value)}` : String(value);
}

/** The time of day, for example "14.32". */
export function formatClock(epochMs: number): string {
  const parts = zonedParts(epochMs);
  return `${pad(parts.hour)}.${pad(parts.minute)}`;
}

/** The date without a year, for example "27.11.". */
export function formatDayMonth(epochMs: number): string {
  const parts = zonedParts(epochMs);
  return `${String(parts.day)}.${String(parts.month)}.`;
}

/** The date with a year, for example "27.11.2026". */
export function formatDate(epochMs: number): string {
  const parts = zonedParts(epochMs);
  return `${String(parts.day)}.${String(parts.month)}.${String(parts.year)}`;
}

/** The date and time, for example "27.11.2026 klo 14.32". */
export function formatDateTime(epochMs: number): string {
  return `${formatDate(epochMs)} ${fi.timeWord} ${formatClock(epochMs)}`;
}

type CalendarDate = { readonly year: number; readonly month: number; readonly day: number };

function sameDate(a: CalendarDate, b: CalendarDate): boolean {
  return a.year === b.year && a.month === b.month && a.day === b.day;
}

/**
 * The calendar date before a date. The arithmetic is on the calendar date
 * itself (`Date.UTC` with day - 1 handles the month and year ends), not on
 * a fixed number of hours: a day in Europe/Helsinki has 23, 24, or 25 hours.
 */
export function previousDate(date: CalendarDate): CalendarDate {
  const before = new Date(Date.UTC(date.year, date.month - 1, date.day - 1));
  return { year: before.getUTCFullYear(), month: before.getUTCMonth() + 1, day: before.getUTCDate() };
}

/**
 * The date and time relative to a reference instant of the server: "tänään
 * klo 14.32", "eilen klo 14.32", or "27.11.2026 klo 14.32". "tänään" and
 * "eilen" compare the calendar dates of Europe/Helsinki.
 */
export function formatDateTimeNear(epochMs: number, referenceMs: number): string {
  const date = zonedParts(epochMs);
  const reference = zonedParts(referenceMs);
  if (sameDate(date, reference)) {
    return `${fi.today} ${fi.timeWord} ${formatClock(epochMs)}`;
  }
  if (sameDate(date, previousDate(reference))) {
    return `${fi.yesterday} ${fi.timeWord} ${formatClock(epochMs)}`;
  }
  return formatDateTime(epochMs);
}

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

/**
 * An elapsed time, not a calendar span: "pv" here is 24 hours of elapsed
 * time, and no day label comes from it. Rounded down to its two largest units: "45 s" is
 * "alle 1 min", then "12 min", "3 h 5 min", "2 pv 4 h". A negative value
 * (the vehicle clock ahead of the server clock) is "alle 1 min".
 */
export function formatDuration(ms: number): string {
  if (ms < MINUTE_MS) {
    return fi.durationUnderMinute;
  }
  const days = Math.floor(ms / DAY_MS);
  const hours = Math.floor((ms % DAY_MS) / HOUR_MS);
  const minutes = Math.floor((ms % HOUR_MS) / MINUTE_MS);
  if (days > 0) {
    return hours > 0 ? `${String(days)} pv ${String(hours)} h` : `${String(days)} pv`;
  }
  if (hours > 0) {
    return minutes > 0 ? `${String(hours)} h ${String(minutes)} min` : `${String(hours)} h`;
  }
  return `${String(minutes)} min`;
}

/** The epoch milliseconds of a validated wire instant. */
export function instantMs(instant: string): number {
  return Date.parse(instant);
}
