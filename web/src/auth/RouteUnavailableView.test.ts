// Renders the unavailable state of a route to static markup and checks
// its accessible structure and its body text.
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { fi } from '../i18n/fi.js';
import { RouteUnavailableView } from './RouteUnavailableView.js';

function render(body: string): string {
  return renderToStaticMarkup(createElement(RouteUnavailableView, { body, onRetry: () => undefined }));
}

describe('RouteUnavailableView', () => {
  it('is a region named by its one heading', () => {
    const html = render(fi.unavailableBody);
    expect(html).toMatch(/^<section aria-labelledby="unavailable-title">/);
    expect(html.match(/<h1/g)?.length).toBe(1);
    expect(html).toContain(`<h1 id="unavailable-title" class="notice-title">${fi.unavailableTitle}</h1>`);
    expect(html).toContain(`<p>${fi.unavailableBody}</p>`);
    expect(html).toContain(`>${fi.retry}</button>`);
  });

  it('shows the fix of two session cookies', () => {
    const html = render(fi.conflictingCookies);
    expect(html).toContain('Poista tämän sivuston evästeet');
  });
});
