import { describe, expect, it } from 'vitest';

import { fi } from '../i18n/fi.js';
import { formatSessionEnd } from './session-end.js';

describe('formatSessionEnd', () => {
  it('shows a winter instant in Finnish time (UTC+2)', () => {
    expect(formatSessionEnd('2026-12-27T08:00:00Z')).toBe('27.12.2026 klo 10.00');
  });

  it('shows a summer instant in Finnish summer time (UTC+3)', () => {
    expect(formatSessionEnd('2026-07-01T09:05:00Z')).toBe('1.7.2026 klo 12.05');
  });

  it('uses the Finnish date across midnight UTC', () => {
    expect(formatSessionEnd('2026-12-31T22:30:00.123456Z')).toBe('1.1.2027 klo 0.30');
  });

  it('returns null for an invalid instant', () => {
    expect(formatSessionEnd('not an instant')).toBeNull();
  });

  it('builds the Finnish sentence', () => {
    expect(fi.sessionEnds('27.12.2026 klo 10.00')).toBe('Istunto päättyy viimeistään 27.12.2026 klo 10.00');
  });
});
