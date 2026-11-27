// Renders the logout failure page to static markup and checks its
// accessible structure.
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { fi } from '../i18n/fi.js';
import { LogoutFailureView } from './LogoutFailureView.js';
import type { LogoutFailureViewProps } from './LogoutFailureView.js';

function render(overrides: Partial<LogoutFailureViewProps> = {}): string {
  const props: LogoutFailureViewProps = {
    pending: false,
    message: fi.logoutFailedBody,
    onRetry: () => undefined,
    ...overrides,
  };
  return renderToStaticMarkup(createElement(LogoutFailureView, props));
}

describe('LogoutFailureView', () => {
  it('has one heading, the alert outside it, and the retry button', () => {
    const html = render();
    expect(html.match(/<h1/g)?.length).toBe(1);
    expect(html).toContain('<h1 id="logout-failure-title">Uloskirjautuminen ei onnistunut</h1>');
    expect(html).toContain(
      '<div role="alert" class="logout-failure-alert"><p class="notice notice-error">Palvelin ei vahvistanut uloskirjautumista. Istunto voi olla yhä voimassa tällä laitteella.</p></div>',
    );
    expect(html).toContain('<button type="button">Yritä uloskirjautumista uudelleen</button>');
    expect(html).not.toContain('aria-labelledby');
    expect(html).toMatch(/^<section class="logout-failure">/);
  });

  it('keeps an empty alert region while a retry runs', () => {
    const html = render({ pending: true, message: null });
    expect(html).toContain('<div role="alert" class="logout-failure-alert"></div>');
    expect(html).toContain('<button type="button" aria-disabled="true">Kirjaudutaan ulos…</button>');
    expect(html).not.toContain(' disabled');
  });
});
