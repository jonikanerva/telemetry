import { describe, expect, it } from 'vitest';

import { MAX_REDIRECT_LENGTH, parseLoginSearch, safeRedirect } from './safe-redirect.js';

const ORIGIN = 'https://telemetry.example';

describe('safeRedirect', () => {
  it.each([
    ['/', '/'],
    ['/battery', '/battery'],
    ['/battery?range=30d', '/battery?range=30d'],
    ['/battery#chart', '/battery#chart'],
    ['/battery?range=30d#chart', '/battery?range=30d#chart'],
    ['/a/b/../c', '/a/c'],
    ['/loginx', '/loginx'],
    ['/battery?next=%2Flogin', '/'],
  ])('maps the internal path %s to %s', (raw, expected) => {
    expect(safeRedirect(raw, ORIGIN)).toBe(expected);
  });

  // Each value must become the start page.
  it.each([
    ['a protocol-relative URL', '//evil.example'],
    ['a protocol-relative URL with a path', '//evil.example/battery'],
    ['three slashes', '///evil.example'],
    ['a dot segment before two slashes', '/.//evil.example'],
    ['a parent segment before two slashes', '/a/..//evil.example'],
    ['a backslash after the slash', '/\\evil.example'],
    ['two backslashes', '\\\\evil.example'],
    ['a slash and a backslash in the middle', '/battery\\..\\'],
    ['an absolute https URL', 'https://evil.example'],
    ['an absolute URL of the same origin', `${ORIGIN}/battery`],
    ['a javascript URL', 'javascript:alert(1)'],
    ['a data URL', 'data:text/html,<script>alert(1)</script>'],
    ['a scheme with leading space', ' https://evil.example'],
    ['encoded slashes', '/%2F%2Fevil.example'],
    ['encoded lower-case slashes', '/%2f%2fevil.example'],
    ['an encoded backslash', '/%5Cevil.example'],
    ['a tab before two slashes', '\t//evil.example'],
    ['a tab between the slashes', '/\t/evil.example'],
    ['a newline between the slashes', '/\n/evil.example'],
    ['a carriage return between the slashes', '/\r/evil.example'],
    ['a NUL character', '/\u0000/evil.example'],
    ['a C1 control', '/\u0085/evil.example'],
    ['a line separator', '/ /evil.example'],
    ['a no-break space', '/ battery'],
    ['a plain space', '/battery now'],
    ['a relative path', 'battery'],
    ['a dot path', './battery'],
    ['a query only', '?redirect=/'],
    ['a hash only', '#top'],
    ['an empty value', ''],
    ['the login page', '/login'],
    ['the login page with a query', '/login?redirect=%2F'],
    ['the login page with a trailing slash', '/login/'],
    ['the login page through dot segments', '/battery/../login'],
    ['a very long path', `/${'a'.repeat(MAX_REDIRECT_LENGTH)}`],
  ])('rejects %s', (_name, raw) => {
    expect(safeRedirect(raw, ORIGIN)).toBe('/');
  });

  it('accepts a path of exactly the maximum length', () => {
    const raw = `/${'a'.repeat(MAX_REDIRECT_LENGTH - 1)}`;
    expect(safeRedirect(raw, ORIGIN)).toBe(raw);
  });

  it.each([
    ['undefined', undefined],
    ['null', null],
    ['a number', 42],
    ['an array', ['/battery']],
    ['an object', { path: '/battery' }],
  ])('rejects %s', (_name, raw) => {
    expect(safeRedirect(raw, ORIGIN)).toBe('/');
  });

  // Property-style check: whatever the input, the result is an internal
  // path that resolves to the page origin and is not the login page.
  it('always returns an internal path of the origin', () => {
    const parts = ['/', '//', '\\', 'evil.example', '%2F', ':', 'javascript:', '\t', '?', '#', 'login', '.', '@', 'a'];
    for (const first of parts) {
      for (const second of parts) {
        for (const third of parts) {
          const result = safeRedirect(first + second + third, ORIGIN);
          expect(result.startsWith('/')).toBe(true);
          expect(result.startsWith('//')).toBe(false);
          expect(new URL(result, ORIGIN).origin).toBe(ORIGIN);
          expect(new URL(result, ORIGIN).pathname).not.toBe('/login');
        }
      }
    }
  });
});

describe('parseLoginSearch', () => {
  it('keeps a missing redirect missing', () => {
    expect(parseLoginSearch({}, ORIGIN)).toEqual({});
  });

  it('keeps a safe redirect', () => {
    expect(parseLoginSearch({ redirect: '/battery?range=7d' }, ORIGIN)).toEqual({ redirect: '/battery?range=7d' });
  });

  it('replaces an unsafe redirect with the start page', () => {
    expect(parseLoginSearch({ redirect: '//evil.example' }, ORIGIN)).toEqual({ redirect: '/' });
    expect(parseLoginSearch({ redirect: 7 }, ORIGIN)).toEqual({ redirect: '/' });
  });
});
