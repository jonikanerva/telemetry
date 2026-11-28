// The contrast test of the design tokens (WCAG 2.2 AA): text 4.5:1 against
// its background; chart lines, markers, the gap hatch, borders of
// controls, and the focus ring 3:1 against the background they lie on.
// The test reads tokens.css itself, in both color schemes.
import { describe, expect, it } from 'vitest';

import tokensCss from './tokens.css?raw';

type Scheme = Map<string, string>;

function declarations(block: string): Scheme {
  const map: Scheme = new Map();
  for (const match of block.matchAll(/--([a-z0-9-]+):\s*(#[0-9a-f]{6})\s*;/gi)) {
    const [, name, value] = match;
    if (name !== undefined && value !== undefined) {
      map.set(name, value.toLowerCase());
    }
  }
  return map;
}

function schemes(): { light: Scheme; dark: Scheme } {
  const darkStart = tokensCss.indexOf('@media (prefers-color-scheme: dark)');
  expect(darkStart).toBeGreaterThan(0);
  const light = declarations(tokensCss.slice(0, darkStart));
  const dark = new Map([...light, ...declarations(tokensCss.slice(darkStart))]);
  return { light, dark };
}

function luminance(hex: string): number {
  const channel = (offset: number): number => {
    const value = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
}

export function contrast(a: string, b: string): number {
  const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return ((high ?? 0) + 0.05) / ((low ?? 0) + 0.05);
}

// [foreground, background]
const TEXT_PAIRS: [string, string][] = [
  ['color-text', 'color-bg'],
  ['color-text-muted', 'color-bg'],
  ['color-text', 'color-surface'],
  ['color-text-muted', 'color-surface'],
  ['color-link', 'color-bg'],
  ['color-notice-text', 'color-notice-bg'],
  ['color-error-text', 'color-error-bg'],
  ['color-info-text', 'color-info-bg'],
  ['color-warning-text', 'color-warning-bg'],
  ['color-button-text', 'color-button-bg'],
  ['color-chart-axis-text', 'color-bg'],
];

const GRAPHIC_PAIRS: [string, string][] = [
  ['color-chart-primary', 'color-bg'],
  ['color-chart-secondary', 'color-bg'],
  ['color-chart-primary', 'color-gap-fill'],
  ['color-chart-secondary', 'color-gap-fill'],
  ['color-gap-hatch', 'color-bg'],
  ['color-gap-hatch', 'color-gap-fill'],
  ['color-chart-crosshair', 'color-bg'],
  ['color-border', 'color-bg'],
  ['color-focus', 'color-bg'],
  ['color-button-bg', 'color-bg'],
  ['color-status-connected', 'color-bg'],
  ['color-status-offline', 'color-bg'],
];

describe('design tokens', () => {
  const all = schemes();
  for (const [name, scheme] of Object.entries(all)) {
    describe(`${name} scheme`, () => {
      it.each(TEXT_PAIRS)('text %s on %s has at least 4.5:1', (fg, bg) => {
        const ratio = contrast(scheme.get(fg) ?? '', scheme.get(bg) ?? '');
        expect(ratio, `${fg} on ${bg}`).toBeGreaterThanOrEqual(4.5);
      });

      it.each(GRAPHIC_PAIRS)('graphic %s on %s has at least 3:1', (fg, bg) => {
        const ratio = contrast(scheme.get(fg) ?? '', scheme.get(bg) ?? '');
        expect(ratio, `${fg} on ${bg}`).toBeGreaterThanOrEqual(3);
      });
    });
  }

  it('defines every tested token in both schemes', () => {
    for (const scheme of Object.values(all)) {
      for (const [fg, bg] of [...TEXT_PAIRS, ...GRAPHIC_PAIRS]) {
        expect(scheme.get(fg), fg).toMatch(/^#[0-9a-f]{6}$/);
        expect(scheme.get(bg), bg).toMatch(/^#[0-9a-f]{6}$/);
      }
    }
  });

  it('computes the reference contrast of black on white', () => {
    expect(contrast('#000000', '#ffffff')).toBeCloseTo(21, 5);
  });
});
