// The smoke journey of the scaffold: the shell around the start page (the
// battery view), the reload banner, the content security policy, and the
// problem response of an unknown API path. The start page needs a
// session (ADR 0012), so these tests log in through the API first.
import process from 'node:process';

import type { Page } from '@playwright/test';

import { loginByApi, resetAuth } from './auth-support.js';
import { SERVER_VERSION, cspViolations, expect, recordCspViolations, recordForeignRequests, test } from './support.js';

test.beforeEach(async ({ page, request, baseURL }) => {
  await resetAuth(request);
  await loginByApi(page, baseURL ?? '');
});

test.describe('shell', () => {
  test('opens the battery view in the shell with no foreign request and no CSP violation', async ({
    page,
    baseURL,
  }) => {
    await recordCspViolations(page);
    const foreign = recordForeignRequests(page, baseURL ?? '');

    const response = await page.goto('/');
    expect(response?.status()).toBe(200);
    expect(response?.headers()['content-security-policy']).toContain("script-src 'self'");
    // The start page is the battery view (its axe check: battery.spec.ts).
    await expect(page).toHaveURL(/\/battery\?range=7d$/);
    await expect(page.getByRole('banner')).toContainText('Telemetria');
    await expect(page.getByRole('heading', { level: 1, name: 'Akku' })).toBeVisible();
    await expect(page.getByText('Ajat ovat Suomen aikaa (Europe/Helsinki).')).toBeVisible();
    // The server filled in the version, so the placeholder is gone.
    await expect(page.locator('meta[name="telemetry-version"]')).toHaveAttribute('content', SERVER_VERSION);
    // Equal versions give no reload banner.
    await expect(page.getByText('Sovelluksesta on uusi versio.')).toHaveCount(0);

    expect(await cspViolations(page)).toEqual([]);
    expect(foreign).toEqual([]);
  });

  test('shows the reload banner when the server runs another version', async ({ page }) => {
    await page.route('**/health', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', json: { status: 'ok', version: 'fedcba9' } }),
    );
    await page.goto('/');
    await expect(page.getByText('Sovelluksesta on uusi versio.')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Lataa sivu uudelleen' })).toBeVisible();
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
  await expect(page.getByRole('heading', { level: 1, name: 'Akku' })).toBeVisible();

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
