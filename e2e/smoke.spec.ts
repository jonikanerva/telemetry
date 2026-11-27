// The smoke journey of the scaffold: the start page, its states, the
// accessibility check in both color schemes, the content security policy,
// and the problem response of an unknown API path. The start page needs a
// session (ADR 0012), so these tests log in through the API first.
import process from 'node:process';

import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

import { loginByApi, resetAuth } from './auth-support.js';
import { SERVER_VERSION, WCAG_AA_TAGS, cspViolations, recordCspViolations, recordForeignRequests } from './support.js';

test.beforeEach(async ({ page, request, baseURL }) => {
  await resetAuth(request);
  await loginByApi(page, baseURL ?? '');
});

async function expectNoAxeViolations(page: Page): Promise<void> {
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme });
    const result = await new AxeBuilder({ page }).withTags(WCAG_AA_TAGS).analyze();
    expect(result.violations, `axe violations in the ${colorScheme} color scheme`).toEqual([]);
  }
}

test.describe('start page', () => {
  test('shows the health state with no foreign request, no CSP violation, and no axe violation', async ({
    page,
    baseURL,
  }) => {
    await recordCspViolations(page);
    const foreign = recordForeignRequests(page, baseURL ?? '');

    const response = await page.goto('/');
    expect(response?.status()).toBe(200);
    expect(response?.headers()['content-security-policy']).toContain("script-src 'self'");
    await expect(page.getByRole('heading', { level: 1, name: 'Telemetria' })).toBeVisible();
    await expect(page.getByText('Palvelu toimii.')).toBeVisible();
    await expect(page.getByText(SERVER_VERSION)).toBeVisible();
    await expect(page.getByText('Ajat ovat Suomen aikaa (Europe/Helsinki).')).toBeVisible();
    // The server filled in the version, so the placeholder is gone.
    await expect(page.locator('meta[name="telemetry-version"]')).toHaveAttribute('content', SERVER_VERSION);
    // Equal versions give no reload banner.
    await expect(page.getByText('Sovelluksesta on uusi versio.')).toHaveCount(0);

    // Read the CSP record before axe adds its script to the page.
    expect(await cspViolations(page)).toEqual([]);
    await expectNoAxeViolations(page);
    expect(foreign).toEqual([]);
  });

  test('shows the unavailable state, not an empty result, when /health fails', async ({ page }) => {
    await page.route('**/health', (route) =>
      route.fulfill({ status: 502, contentType: 'text/html', body: '<html>Bad Gateway</html>' }),
    );
    await page.goto('/');
    await expect(page.getByText('Tietoja ei saatu')).toBeVisible();
    await expect(page.getByText('Palvelu toimii.')).toHaveCount(0);
    await expectNoAxeViolations(page);
  });

  test('shows the reload banner when the server runs another version', async ({ page }) => {
    await page.route('**/health', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', json: { status: 'ok', version: 'fedcba9' } }),
    );
    await page.goto('/');
    await expect(page.getByText('Sovelluksesta on uusi versio.')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Lataa sivu uudelleen' })).toBeVisible();
    await expectNoAxeViolations(page);
  });

  test('shows the not-found page for an unknown path', async ({ page }) => {
    const response = await page.goto('/no-such-page');
    expect(response?.status()).toBe(200);
    await expect(page.getByRole('heading', { level: 1, name: 'Sivua ei löydy' })).toBeVisible();
  });
});

test('an unknown API path gives a problem response', async ({ request }) => {
  const response = await request.get('/api/v1/no-such-route');
  expect(response.status()).toBe(404);
  expect(response.headers()['content-type']).toMatch(/^application\/problem\+json/);
  expect(response.headers()['cache-control']).toBe('no-store');
  const body: unknown = await response.json();
  expect(body).toMatchObject({ type: 'about:blank', status: 404, code: 'not_found' });
});

test('a missing asset gives a 404, not index.html', async ({ request }) => {
  const response = await request.get('/assets/no-such-file.js');
  expect(response.status()).toBe(404);
  expect(response.headers()['content-type']).toMatch(/^application\/problem\+json/);
});

test('the keyboard reaches the controls with a visible focus', async ({ page, browserName }) => {
  await page.goto('/');
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();

  // WebKit on macOS moves the focus with Tab only between text fields, as
  // Safari does by default; Option+Tab moves it to every element.
  const tab = browserName === 'webkit' && process.platform === 'darwin' ? 'Alt+Tab' : 'Tab';

  await page.keyboard.press(tab);
  const skipLink = page.getByRole('link', { name: 'Siirry sisältöön' });
  await expect(skipLink).toBeFocused();
  await expect(skipLink).toBeInViewport();
  await expectVisibleFocus(page);
  await page.keyboard.press('Enter');
  await expect(page.locator('main')).toBeFocused();

  const refresh = page.getByRole('button', { name: 'Päivitä' });
  for (let presses = 0; presses < 10; presses += 1) {
    if (await refresh.evaluate((element) => element === document.activeElement)) {
      break;
    }
    await page.keyboard.press(tab);
  }
  await expect(refresh).toBeFocused();
  await expectVisibleFocus(page);
});

async function expectVisibleFocus(page: Page): Promise<void> {
  const outline = await page.evaluate(() => {
    const element = document.activeElement;
    if (element === null) {
      return null;
    }
    const style = window.getComputedStyle(element);
    return { style: style.outlineStyle, width: Number.parseFloat(style.outlineWidth) };
  });
  expect(outline?.style).not.toBe('none');
  expect(outline?.width ?? 0).toBeGreaterThanOrEqual(2);
}
