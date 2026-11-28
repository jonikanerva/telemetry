// The range of the battery view in the URL: `/battery?range=<key>`.
//
// Rule: the URL owns the range (profile §2.4). A missing or unknown value
// is replaced with the default `7d`, and the replacement does not add a
// history entry.
import type { RangeKey } from 'telemetry-web-api-client';

export const RANGE_KEYS = ['24h', '7d', '30d', '90d', '1y'] as const satisfies readonly RangeKey[];

export const DEFAULT_RANGE: RangeKey = '7d';

export type BatterySearch = { readonly range: RangeKey };

export function parseRange(value: unknown): RangeKey | null {
  return RANGE_KEYS.find((key) => key === value) ?? null;
}

/** The validated search of the route. Unknown keys of the URL are dropped. */
export function validateBatterySearch(search: Record<string, unknown>): BatterySearch {
  return { range: parseRange(search['range']) ?? DEFAULT_RANGE };
}

/** True when the raw URL search holds exactly a valid range and nothing else. */
export function isCanonicalSearch(search: Record<string, unknown>): boolean {
  const keys = Object.keys(search);
  return keys.length === 1 && keys[0] === 'range' && parseRange(search['range']) !== null;
}
