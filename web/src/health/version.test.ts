import { describe, expect, it } from 'vitest';

import { needsReload, parseVersion } from './version.js';

const A = parseVersion('0123456789abcdef0123456789abcdef01234567');
const B = parseVersion('fedcba9876543210fedcba9876543210fedcba98');

describe('parseVersion', () => {
  it.each(['abc1234', '0123456789abcdef0123456789abcdef01234567'])('accepts the SHA %s', (raw) => {
    expect(parseVersion(raw)).toEqual({ kind: 'known', sha: raw });
  });

  it.each([
    ['the unreplaced placeholder', '__TELEMETRY_VERSION__'],
    ['the server value unknown', 'unknown'],
    ['an empty value', ''],
    ['a short value', 'abc123'],
    ['a value that is too long', '0123456789abcdef0123456789abcdef012345678'],
    ['upper case', 'ABC1234'],
    ['a value with spaces', ' abc1234'],
  ])('treats %s as unknown', (_name, raw) => {
    expect(parseVersion(raw)).toEqual({ kind: 'unknown' });
  });

  it('treats a missing value as unknown', () => {
    expect(parseVersion(null)).toEqual({ kind: 'unknown' });
    expect(parseVersion(undefined)).toEqual({ kind: 'unknown' });
  });
});

describe('needsReload', () => {
  const unknown = parseVersion('unknown');

  it('is false for equal versions', () => {
    expect(needsReload(A, A)).toBe(false);
  });

  it('is true for two different known versions', () => {
    expect(needsReload(A, B)).toBe(true);
  });

  it('is false when either side is unknown', () => {
    expect(needsReload(A, unknown)).toBe(false);
    expect(needsReload(unknown, A)).toBe(false);
    expect(needsReload(unknown, unknown)).toBe(false);
  });
});
