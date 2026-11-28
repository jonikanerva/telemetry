// Renders the login form to static markup and checks its accessible
// structure. The focus move after a failure needs a browser; the
// end-to-end journeys check it.
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { LoginFormView } from './LoginFormView.js';
import type { LoginFormViewProps } from './LoginFormView.js';

function render(overrides: Partial<LoginFormViewProps> = {}): string {
  const props: LoginFormViewProps = {
    password: '',
    pending: false,
    message: null,
    onPasswordChange: () => undefined,
    onSubmit: () => undefined,
    ...overrides,
  };
  return renderToStaticMarkup(createElement(LoginFormView, props));
}

function count(html: string, pattern: RegExp): number {
  return html.match(pattern)?.length ?? 0;
}

describe('LoginFormView', () => {
  it('has a heading, a labelled password field, and a submit button', () => {
    const html = render();
    expect(html).toContain('<h1 id="login-title">Kirjaudu sisään</h1>');
    expect(html).toMatch(/^<section class="login" aria-labelledby="login-title">/);
    expect(html).toContain('<label for="login-password">Salasana</label>');
    expect(html).toMatch(/<input[^>]* id="login-password"/);
    expect(html).toMatch(/<input[^>]* type="password"/);
    expect(html).toMatch(/<input[^>]* autoComplete="current-password"/);
    expect(html).toMatch(/<input[^>]* required=""/);
    expect(html).toMatch(/<input[^>]* maxLength="1024"/);
    expect(html).toMatch(/<button type="submit">Kirjaudu sisään<\/button>/);
    expect(html).toMatch(/<form[^>]*>/);
  });

  it('keeps the alert region outside the busy form', () => {
    expect(render()).toMatch(/<div role="alert"[^>]*><\/div><form /);
  });

  it('has one empty alert region and no error state without a failure', () => {
    const html = render();
    expect(count(html, /role="alert"/g)).toBe(1);
    expect(html).toContain('<div role="alert" class="login-alert"></div>');
    expect(html).not.toContain('aria-invalid');
    expect(html).not.toContain('aria-describedby');
    expect(html).toContain('aria-busy="false"');
  });

  it('marks the field invalid and links the message after a failure', () => {
    const html = render({ message: 'Salasana on väärä.' });
    expect(count(html, /role="alert"/g)).toBe(1);
    expect(html).toMatch(/<div role="alert"[^>]*><p id="login-message"[^>]*>Salasana on väärä\.<\/p><\/div>/);
    expect(html).toMatch(/<input[^>]* aria-invalid="true"/);
    expect(html).toMatch(/<input[^>]* aria-describedby="login-message"/);
  });

  it('shows the busy state while the request runs', () => {
    const html = render({ pending: true, password: 'secret' });
    expect(html).toContain('aria-busy="true"');
    expect(html).toMatch(/<button type="submit" aria-disabled="true">Kirjaudutaan…<\/button>/);
  });

  it('uses no inline style', () => {
    expect(render({ message: 'x', pending: true })).not.toContain('style=');
  });
});
