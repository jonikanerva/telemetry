// Reduced motion (WCAG 2.2 AA 2.3.3 is AAA; the profile asks for the
// setting to be respected): the stylesheets of the SPA define no
// animation and no transition, so `prefers-reduced-motion` has nothing
// to turn off. A new animation must come with a reduced-motion rule and
// a change of this test.
import { describe, expect, it } from 'vitest';

import appCss from './app.css?raw';
import tokensCss from './tokens.css?raw';
import batteryCss from '../battery/battery.css?raw';
import chartCss from '../chart/chart.css?raw';

describe('motion', () => {
  it.each([
    ['app.css', appCss],
    ['tokens.css', tokensCss],
    ['battery.css', batteryCss],
    ['chart.css', chartCss],
  ])('%s defines no animation and no transition', (_name, css) => {
    expect(css.length).toBeGreaterThan(0);
    expect(css).not.toMatch(/@keyframes|animation|transition|scroll-behavior:\s*smooth/);
  });
});
