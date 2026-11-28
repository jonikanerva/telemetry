import { describe, expect, it } from 'vitest';

import {
  formatClock,
  formatDate,
  formatDateTimeNear,
  formatDayMonth,
  formatDuration,
  formatNumber,
  formatSigned,
  formatValue,
  previousDate,
  zonedParts,
} from './format.js';

const at = (iso: string): number => Date.parse(iso);
const percent = { symbol: 'percent', source: 'documented' } as const;
const celsius = { symbol: 'degree_celsius', source: 'assumed' } as const;

describe('numbers', () => {
  it('uses the decimal comma and the Finnish minus sign', () => {
    expect(formatNumber(58.25)).toBe('58,3');
    expect(formatNumber(-3.5)).toBe('−3,5');
    expect(formatNumber(80)).toBe('80');
    expect(formatNumber(1234.5, 1)).toBe('1 234,5');
  });

  it('keeps more decimals when asked, and never shows a missing value as zero', () => {
    expect(formatNumber(75.312, 3)).toBe('75,312');
    expect(formatValue(80.5, percent)).toBe('80,5 %');
    expect(formatValue(-3, celsius)).toBe('−3 °C');
  });

  it('signs a change', () => {
    expect(formatSigned(-0.3)).toBe('−0,3');
    expect(formatSigned(0.25)).toBe('+0,3');
    expect(formatSigned(0)).toBe('0');
    expect(formatSigned(-0.01)).toBe('0');
  });
});

describe('time in Europe/Helsinki', () => {
  it('shows the time of Helsinki, whatever the zone of the host', () => {
    // 12:00 UTC in November is 14:00 in Helsinki (EET, UTC+2).
    expect(formatClock(at('2026-11-27T12:00:00Z'))).toBe('14.00');
    expect(formatDayMonth(at('2026-11-27T22:30:00Z'))).toBe('28.11.');
    expect(formatDate(at('2026-11-27T21:59:00Z'))).toBe('27.11.2026');
  });

  it('follows the change from summer time on 25 October 2026', () => {
    // 00:30 UTC is 03:30 EEST (UTC+3); 01:30 UTC is 03:30 EET (UTC+2).
    expect(formatClock(at('2026-10-25T00:30:00Z'))).toBe('03.30');
    expect(formatClock(at('2026-10-25T01:30:00Z'))).toBe('03.30');
    expect(zonedParts(at('2026-10-24T21:00:00Z'))).toEqual({ year: 2026, month: 10, day: 25, hour: 0, minute: 0 });
    expect(zonedParts(at('2026-10-25T22:00:00Z'))).toEqual({ year: 2026, month: 10, day: 26, hour: 0, minute: 0 });
  });

  it('follows the change to summer time on 28 March 2027', () => {
    // 00:59 UTC is 02:59 EET; 01:00 UTC is 04:00 EEST.
    expect(formatClock(at('2027-03-28T00:59:00Z'))).toBe('02.59');
    expect(formatClock(at('2027-03-28T01:00:00Z'))).toBe('04.00');
  });

  it('names today and yesterday by the days of Helsinki', () => {
    const now = at('2026-11-27T12:00:00Z');
    expect(formatDateTimeNear(at('2026-11-27T11:50:00Z'), now)).toBe('tänään klo 13.50');
    // 22:30 UTC on 26 November is already 27 November in Helsinki.
    expect(formatDateTimeNear(at('2026-11-26T22:30:00Z'), now)).toBe('tänään klo 00.30');
    expect(formatDateTimeNear(at('2026-11-26T21:30:00Z'), now)).toBe('eilen klo 23.30');
    expect(formatDateTimeNear(at('2026-11-25T21:30:00Z'), now)).toBe('25.11.2026 klo 23.30');
  });

  it('names yesterday across a DST change', () => {
    // On 26 October 2026 at 00:30 Helsinki time, yesterday was the 25th, a 25-hour day.
    const now = at('2026-10-25T22:30:00Z');
    expect(formatDateTimeNear(at('2026-10-24T21:30:00Z'), now)).toBe('eilen klo 00.30');
    expect(formatDateTimeNear(at('2026-10-24T20:30:00Z'), now)).toBe('24.10.2026 klo 23.30');
  });
});

describe('durations', () => {
  it('rounds down to its two largest units', () => {
    expect(formatDuration(0)).toBe('alle 1 min');
    expect(formatDuration(59_999)).toBe('alle 1 min');
    expect(formatDuration(12 * 60_000 + 30_000)).toBe('12 min');
    expect(formatDuration(3 * 3_600_000)).toBe('3 h');
    expect(formatDuration(3 * 3_600_000 + 5 * 60_000)).toBe('3 h 5 min');
    expect(formatDuration(26 * 3_600_000)).toBe('1 pv 2 h');
    expect(formatDuration(48 * 3_600_000)).toBe('2 pv');
  });

  it('shows a negative age (vehicle clock ahead) as under a minute', () => {
    expect(formatDuration(-90_000)).toBe('alle 1 min');
  });
});

// A table of reference days. Each row: the Helsinki date, its first
// instant (local midnight), the first instant of the next day, and three
// instants at local noon one, two, and three calendar days back with
// their labels. All values are written by hand from the EET (UTC+2) and
// EEST (UTC+3) offsets; the test does not compute them.
type DayRow = {
  readonly name: string;
  readonly start: string;
  readonly end: string;
  readonly hours: number;
  readonly back: readonly (readonly [string, string])[];
};

const DAY_ROWS: readonly DayRow[] = [
  {
    name: '28.11.2026, an ordinary day',
    start: '2026-11-27T22:00:00Z',
    end: '2026-11-28T22:00:00Z',
    hours: 24,
    back: [
      ['2026-11-27T10:00:00Z', 'eilen klo 12.00'],
      ['2026-11-26T22:00:00Z', 'eilen klo 00.00'],
      ['2026-11-26T21:59:00Z', '26.11.2026 klo 23.59'],
      ['2026-11-26T10:00:00Z', '26.11.2026 klo 12.00'],
      ['2026-11-25T10:00:00Z', '25.11.2026 klo 12.00'],
    ],
  },
  {
    name: '24.10.2026, the day before the end of summer time',
    start: '2026-10-23T21:00:00Z',
    end: '2026-10-24T21:00:00Z',
    hours: 24,
    back: [
      ['2026-10-23T09:00:00Z', 'eilen klo 12.00'],
      ['2026-10-22T09:00:00Z', '22.10.2026 klo 12.00'],
      ['2026-10-21T09:00:00Z', '21.10.2026 klo 12.00'],
    ],
  },
  {
    name: '25.10.2026, the end of summer time (25 hours)',
    start: '2026-10-24T21:00:00Z',
    end: '2026-10-25T22:00:00Z',
    hours: 25,
    back: [
      ['2026-10-24T09:00:00Z', 'eilen klo 12.00'],
      ['2026-10-23T09:00:00Z', '23.10.2026 klo 12.00'],
      ['2026-10-22T09:00:00Z', '22.10.2026 klo 12.00'],
    ],
  },
  {
    name: '26.10.2026, the day after the end of summer time',
    start: '2026-10-25T22:00:00Z',
    end: '2026-10-26T22:00:00Z',
    hours: 24,
    back: [
      ['2026-10-25T10:00:00Z', 'eilen klo 12.00'],
      ['2026-10-24T21:00:00Z', 'eilen klo 00.00'],
      ['2026-10-24T09:00:00Z', '24.10.2026 klo 12.00'],
      ['2026-10-23T09:00:00Z', '23.10.2026 klo 12.00'],
    ],
  },
  {
    name: '27.3.2027, the day before the start of summer time',
    start: '2027-03-26T22:00:00Z',
    end: '2027-03-27T22:00:00Z',
    hours: 24,
    back: [
      ['2027-03-26T10:00:00Z', 'eilen klo 12.00'],
      ['2027-03-25T10:00:00Z', '25.3.2027 klo 12.00'],
      ['2027-03-24T10:00:00Z', '24.3.2027 klo 12.00'],
    ],
  },
  {
    name: '28.3.2027, the start of summer time (23 hours)',
    start: '2027-03-27T22:00:00Z',
    end: '2027-03-28T21:00:00Z',
    hours: 23,
    back: [
      ['2027-03-27T10:00:00Z', 'eilen klo 12.00'],
      ['2027-03-26T10:00:00Z', '26.3.2027 klo 12.00'],
      ['2027-03-25T10:00:00Z', '25.3.2027 klo 12.00'],
    ],
  },
  {
    name: '29.3.2027, the day after the start of summer time',
    start: '2027-03-28T21:00:00Z',
    end: '2027-03-29T21:00:00Z',
    hours: 24,
    back: [
      ['2027-03-28T09:00:00Z', 'eilen klo 12.00'],
      ['2027-03-27T22:00:00Z', 'eilen klo 00.00'],
      ['2027-03-27T10:00:00Z', '27.3.2027 klo 12.00'],
      ['2027-03-26T10:00:00Z', '26.3.2027 klo 12.00'],
    ],
  },
  {
    name: '1.1.2027, a year boundary',
    start: '2026-12-31T22:00:00Z',
    end: '2027-01-01T22:00:00Z',
    hours: 24,
    back: [
      ['2026-12-31T10:00:00Z', 'eilen klo 12.00'],
      ['2026-12-30T10:00:00Z', '30.12.2026 klo 12.00'],
      ['2026-12-29T10:00:00Z', '29.12.2026 klo 12.00'],
    ],
  },
];

describe('the day word against each hour of a reference day', () => {
  for (const row of DAY_ROWS) {
    it(`names the days back correctly for each reference hour of ${row.name}`, () => {
      const start = at(row.start);
      const end = at(row.end);
      // The row states the length of the day by hand; the end is exclusive.
      expect(end - start).toBe(row.hours * 3_600_000);
      // Each whole hour from local midnight, and the last minute of the day.
      const references = [...Array.from({ length: row.hours }, (_, hour) => start + hour * 3_600_000), end - 60_000];
      for (const reference of references) {
        for (const [instant, label] of row.back) {
          expect(formatDateTimeNear(at(instant), reference), `${instant} at ${new Date(reference).toISOString()}`).toBe(
            label,
          );
        }
        // The local midnight of the day itself is "tänään".
        expect(formatDateTimeNear(start, reference)).toBe('tänään klo 00.00');
      }
    });
  }

  it('reproduces the case of the review: 00.30 on 28.11. and noon of 26.11.', () => {
    expect(formatDateTimeNear(at('2026-11-26T10:00:00Z'), at('2026-11-27T22:30:00Z'))).toBe('26.11.2026 klo 12.00');
    expect(formatDateTimeNear(at('2026-11-26T22:00:00Z'), at('2026-11-27T22:30:00Z'))).toBe('eilen klo 00.00');
  });
});

describe('previousDate', () => {
  it.each([
    [
      { year: 2026, month: 11, day: 28 },
      { year: 2026, month: 11, day: 27 },
    ],
    [
      { year: 2026, month: 12, day: 1 },
      { year: 2026, month: 11, day: 30 },
    ],
    [
      { year: 2027, month: 1, day: 1 },
      { year: 2026, month: 12, day: 31 },
    ],
    [
      { year: 2028, month: 3, day: 1 },
      { year: 2028, month: 2, day: 29 },
    ],
    [
      { year: 2027, month: 3, day: 1 },
      { year: 2027, month: 2, day: 28 },
    ],
  ])('gives the calendar day before %o', (date, expected) => {
    expect(previousDate(date)).toEqual(expected);
  });
});
