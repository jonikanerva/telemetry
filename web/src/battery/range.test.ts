import { describe, expect, it } from 'vitest';

import { DEFAULT_RANGE, isCanonicalSearch, parseRange, validateBatterySearch } from './range.js';

describe('the range in the URL', () => {
  it.each(['24h', '7d', '30d', '90d', '1y'])('accepts %s', (key) => {
    expect(validateBatterySearch({ range: key })).toEqual({ range: key });
    expect(isCanonicalSearch({ range: key })).toBe(true);
  });

  it.each([
    ['a missing range', {}],
    ['an unknown range', { range: '2d' }],
    ['a number', { range: 7 }],
    ['an upper-case key', { range: '7D' }],
    ['an array', { range: ['7d'] }],
  ])('replaces %s with the default', (_name, search) => {
    expect(validateBatterySearch(search)).toEqual({ range: DEFAULT_RANGE });
    expect(isCanonicalSearch(search)).toBe(false);
  });

  it('drops unknown keys', () => {
    expect(validateBatterySearch({ range: '30d', extra: 'x' })).toEqual({ range: '30d' });
    expect(isCanonicalSearch({ range: '30d', extra: 'x' })).toBe(false);
  });

  it('has the default 7d', () => {
    expect(DEFAULT_RANGE).toBe('7d');
    expect(parseRange('1y')).toBe('1y');
    expect(parseRange(null)).toBeNull();
  });
});
