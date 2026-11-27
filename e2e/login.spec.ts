// The login journeys of ADR 0003 and ADR 0012, in each browser engine,
// against the real server router, middleware, session store, throttle,
// and Argon2id verification (the test server).
//
// Each test starts from a known server state (resetAuth). The expected
// texts come from the Finnish user interface (web/src/i18n/fi.ts); the
// expected server behaviour comes from ADR 0012.
import process from 'node:process';

import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import type { APIRequestContext, BrowserContext, Page } from '@playwright/test';

import { SESSION_COOKIE, TEST_PASSWORD, advanceClock, authStats, loginByApi, resetAuth } from './auth-support.js';
import { WCAG_AA_TAGS, cspViolations, recordCspViolations, recordForeignRequests } from './support.js';

const SEVEN_DAYS = 7 * 86_400;

test.beforeEach(async ({ request }) => {
  await resetAuth(request);
});

async function expectNoAxeViolations(page: Page): Promise<void> {
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme });
    const result = await new AxeBuilder({ page }).withTags(WCAG_AA_TAGS).analyze();
    expect(result.violations, `axe violations in the ${colorScheme} color scheme`).toEqual([]);
  }
}

function passwordField(page: Page) {
  return page.getByLabel('Salasana', { exact: true });
}

async function submitPassword(page: Page, password: string): Promise<void> {
  await passwordField(page).fill(password);
  await page.getByRole('button', { name: 'Kirjaudu sisään' }).click();
}

async function sessionCookie(context: BrowserContext) {
  const cookies = await context.cookies();
  return cookies.find((cookie) => cookie.name === SESSION_COOKIE);
}

/** The session state on the server for the cookies of the context. */
async function serverSessionStatus(request: APIRequestContext): Promise<number> {
  const response = await request.get('/api/v1/session');
  return response.status();
}

test('login, protected page, logout, and the back button shows no data', async ({ page, context, baseURL }) => {
  await recordCspViolations(page);
  const foreign = recordForeignRequests(page, baseURL ?? '');

  // A protected page without a session leads to the login page.
  await page.goto('/');
  await expect(page).toHaveURL(/\/login\?redirect=%2F$/);
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
  // The protected data never appeared.
  await expect(page.getByText('Palvelu toimii.')).toHaveCount(0);

  await submitPassword(page, TEST_PASSWORD);
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole('heading', { level: 1, name: 'Telemetria' })).toBeVisible();
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();
  await expect(page.getByText(/Istunto päättyy viimeistään/)).toBeVisible();

  const cookie = await sessionCookie(context);
  expect(cookie).toBeDefined();
  expect(cookie?.httpOnly).toBe(true);
  expect(cookie?.secure).toBe(true);
  expect(cookie?.sameSite).toBe('Strict');
  expect(cookie?.path).toBe('/');
  // The page scripts cannot read the session cookie.
  expect(await page.evaluate(() => document.cookie)).not.toContain(SESSION_COOKIE);

  await page.getByRole('button', { name: 'Kirjaudu ulos' }).click();
  await expect(page).toHaveURL(/\/login/);
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
  expect(await sessionCookie(context)).toBeUndefined();
  expect(await serverSessionStatus(page.request)).toBe(401);

  // The back button returns to the protected route, which needs a session
  // again: the login page, not the earlier data.
  await page.goBack();
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
  await expect(page.getByText('Palvelu toimii.')).toHaveCount(0);
  await expect(page.getByText(/Istunto päättyy viimeistään/)).toHaveCount(0);

  expect(await cspViolations(page)).toEqual([]);
  expect(foreign).toEqual([]);
});

test('a stolen copy of the cookie stops working after logout', async ({ page, context, baseURL, playwright }) => {
  await loginByApi(page, baseURL ?? '');
  const cookie = await sessionCookie(context);
  expect(cookie).toBeDefined();
  const copy = `${SESSION_COOKIE}=${cookie?.value ?? ''}`;
  const thief = await playwright.request.newContext({
    baseURL: baseURL ?? '',
    ignoreHTTPSErrors: true,
    extraHTTPHeaders: { cookie: copy },
  });
  expect(await serverSessionStatus(thief)).toBe(200);

  await page.goto('/');
  await page.getByRole('button', { name: 'Kirjaudu ulos' }).click();
  await expect(page).toHaveURL(/\/login/);
  expect(await serverSessionStatus(thief)).toBe(401);
  await thief.dispose();
});

test('a logout that does not reach the server says so, hides the data, and can be retried', async ({
  page,
  baseURL,
}) => {
  await recordCspViolations(page);
  await loginByApi(page, baseURL ?? '');
  await page.goto('/');
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();

  // The DELETE never reaches the server.
  await page.route('**/api/v1/session', (route) =>
    route.request().method() === 'DELETE' ? route.abort('failed') : route.fallback(),
  );
  await page.getByRole('button', { name: 'Kirjaudu ulos' }).click();
  await expect(page).toHaveURL(/\/logout$/);
  await expect(page.getByRole('heading', { level: 1, name: 'Uloskirjautuminen ei onnistunut' })).toBeVisible();
  await expect(page.getByRole('alert')).toHaveText(
    'Palvelin ei vahvistanut uloskirjautumista. Istunto voi olla yhä voimassa tällä laitteella.',
  );
  // No protected data and no logout button of the header in this state.
  await expect(page.getByText('Palvelu toimii.')).toHaveCount(0);
  await expect(page.getByText(/Istunto päättyy viimeistään/)).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Kirjaudu ulos' })).toHaveCount(0);
  // The page tells the truth: the server session is still valid.
  expect(await serverSessionStatus(page.request)).toBe(200);
  expect(await cspViolations(page)).toEqual([]);
  await expectNoAxeViolations(page);

  // A retry that fails again keeps the page.
  const retry = page.getByRole('button', { name: 'Yritä uloskirjautumista uudelleen' });
  await retry.click();
  await expect(page.getByRole('alert')).toContainText('Palvelin ei vahvistanut uloskirjautumista.');
  await expect(page).toHaveURL(/\/logout$/);
  expect(await serverSessionStatus(page.request)).toBe(200);

  // With the server reachable, one retry ends the session.
  await page.unroute('**/api/v1/session');
  await retry.click();
  await expect(page).toHaveURL(/\/login/);
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
  expect(await serverSessionStatus(page.request)).toBe(401);
});

test('Back after a logout in another tab and a full page load shows no data', async ({ page, context, baseURL }) => {
  await loginByApi(page, baseURL ?? '');
  await page.goto('/');
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();
  // A full page load of another document, so Back is a cross-document
  // navigation. This test proves the result of that Back when the browser
  // loads the page again: the route check asks the server. It does not
  // prove the `pageshow` reload of a page from the back-forward cache: the
  // test passed also without the listener, so the cache is probably not
  // active under Playwright. The listener has unit tests
  // (web/src/page-show.test.ts); the cache path in a real browser is a
  // post-release check (docs/web-release.md).
  await page.goto('/no-such-page');
  await expect(page.getByRole('heading', { level: 1, name: 'Sivua ei löydy' })).toBeVisible();

  // Another tab of the same browser logs out.
  const other = await context.newPage();
  const logout = await other.request.delete('/api/v1/session', {
    headers: { origin: new URL(baseURL ?? '').origin },
  });
  expect(logout.status()).toBe(204);
  await other.close();

  await page.goBack();
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
  await expect(page.getByText('Palvelu toimii.')).toHaveCount(0);
  await expect(page.getByText(/Istunto päättyy viimeistään/)).toHaveCount(0);
});

test('/logout opened directly claims no failure and goes to the page of the session state', async ({
  page,
  baseURL,
}) => {
  // Without a session: the login page.
  await page.goto('/logout');
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
  await expect(page.getByText('Uloskirjautuminen ei onnistunut')).toHaveCount(0);

  // With a session: the application, and the session stays.
  await loginByApi(page, baseURL ?? '');
  await page.goto('/logout');
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByText('Uloskirjautuminen ei onnistunut')).toHaveCount(0);
  expect(await serverSessionStatus(page.request)).toBe(200);
});

test('a wrong password shows an announced error and clears the field', async ({ page }) => {
  await page.goto('/login');
  await submitPassword(page, 'wrong password, test only');
  const alert = page.getByRole('alert');
  await expect(alert).toHaveText('Salasana on väärä.');
  await expect(passwordField(page)).toHaveValue('');
  await expect(passwordField(page)).toHaveAttribute('aria-invalid', 'true');
  await expect(passwordField(page)).toBeFocused();
  await expect(page).toHaveURL(/\/login/);
  await expectNoAxeViolations(page);
});

test('the sixth attempt is throttled with the wait time and keeps the input', async ({ page }) => {
  await page.goto('/login');
  for (let attempt = 0; attempt < 5; attempt += 1) {
    await submitPassword(page, `wrong password ${String(attempt)}`);
    await expect(page.getByRole('alert')).toHaveText('Salasana on väärä.');
  }
  // Also the correct password waits: the limit is global (ADR 0003).
  await submitPassword(page, TEST_PASSWORD);
  const alert = page.getByRole('alert');
  await expect(alert).toContainText('Liian monta kirjautumisyritystä.');
  await expect(alert).toContainText(/Yritä uudelleen 1?\d sekunnin kuluttua\./);
  await expect(passwordField(page)).toHaveValue(TEST_PASSWORD);
  await expect(page).toHaveURL(/\/login/);
  await expectNoAxeViolations(page);
});

test('the server unavailable state keeps the input', async ({ page }) => {
  await page.goto('/login');
  await page.route('**/api/v1/session', (route) =>
    route.request().method() === 'POST'
      ? route.fulfill({ status: 502, contentType: 'text/html', body: '<html>Bad Gateway</html>' })
      : route.fallback(),
  );
  await submitPassword(page, TEST_PASSWORD);
  await expect(page.getByRole('alert')).toContainText('Kirjautuminen ei onnistunut.');
  await expect(passwordField(page)).toHaveValue(TEST_PASSWORD);
  await expectNoAxeViolations(page);
});

test('an expired session leads to the login page and back to the page', async ({ page, context, baseURL }) => {
  await loginByApi(page, baseURL ?? '');
  await page.goto('/');
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();

  // Seven days without a request end the session (idle limit).
  await advanceClock(page.request, SEVEN_DAYS);
  await page.reload();
  await expect(page).toHaveURL(/\/login\?redirect=%2F$/);
  await expect(page.getByText('Palvelu toimii.')).toHaveCount(0);
  // A 401 changes no cookie (ADR 0012): the dead cookie stays, and the
  // server refuses it.
  const dead = await sessionCookie(context);
  expect(dead).toBeDefined();
  expect(await serverSessionStatus(page.request)).toBe(401);

  await submitPassword(page, TEST_PASSWORD);
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();
  // The login replaced the dead cookie with a new one.
  const fresh = await sessionCookie(context);
  expect(fresh?.value).not.toBe(dead?.value);
});

test('a login with a hostile redirect stays on this origin', async ({ page, baseURL }) => {
  await page.goto('/login?redirect=%2F%2Fevil.example%2Fpath');
  await submitPassword(page, TEST_PASSWORD);
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();
  expect(new URL(page.url()).origin).toBe(new URL(baseURL ?? '').origin);
});

test.describe('requests from another origin', () => {
  const attacker = 'https://attacker.test';

  test('a cross-site request starts no password check and keeps the session (server 403 in firefox and webkit; chromium blocks it before the server)', async ({
    page,
    context,
    baseURL,
    browserName,
  }) => {
    // The victim has a session.
    await loginByApi(page, baseURL ?? '');
    const victim = await sessionCookie(context);
    expect(victim).toBeDefined();
    const before = await authStats(page.request);

    // A page of another site sends the only state-changing request that a
    // browser allows without a CORS preflight: a "simple" POST with a text
    // body. The browser adds `Origin: https://attacker.test`.
    const target = `${baseURL ?? ''}/api/v1/session`;
    await page.route(`${attacker}/**`, (route) =>
      route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><title>attacker</title>' }),
    );
    await page.goto(`${attacker}/`);
    // The request ends as failed (the page cannot read the response) or,
    // in an engine that reports it so, as finished.
    const isTarget = (request: { url: () => string }) => request.url() === target;
    const settled = Promise.race([
      page.waitForEvent('requestfailed', isTarget).catch(() => undefined),
      page.waitForEvent('requestfinished', isTarget).catch(() => undefined),
    ]);
    await page.evaluate(
      async ({ url, password }) => {
        // The page can never read the response: the server sends
        // `Cross-Origin-Resource-Policy: same-origin`, so the fetch rejects.
        await fetch(url, {
          method: 'POST',
          mode: 'no-cors',
          credentials: 'include',
          headers: { 'content-type': 'text/plain' },
          body: JSON.stringify({ password }),
        }).catch(() => undefined);
      },
      { url: target, password: TEST_PASSWORD },
    );
    await settled;

    // Firefox and WebKit send the request, and the origin rule refuses it.
    // Chromium failed the request with net::ERR_FAILED before it reached
    // the server in the local runs (probably its block of requests from a
    // public site to localhost); a Chromium that sends it gets the same
    // refusal. In every engine, no password check starts and the session
    // stays.
    const after = await authStats(page.request);
    expect(after.verifications).toBe(before.verifications);
    expect(after.refused_origin - before.refused_origin).toBeLessThanOrEqual(1);
    if (browserName !== 'chromium') {
      expect(after.refused_origin - before.refused_origin).toBe(1);
    }
    expect((await sessionCookie(context))?.value).toBe(victim?.value);
    expect(await serverSessionStatus(page.request)).toBe(200);
  });

  test('a forged logout with the cookie and another Origin keeps the session', async ({ page, context, baseURL }) => {
    await loginByApi(page, baseURL ?? '');
    const forged = await page.request.delete('/api/v1/session', { headers: { origin: attacker } });
    expect(forged.status()).toBe(403);
    expect(await serverSessionStatus(page.request)).toBe(200);
    expect(await sessionCookie(context)).toBeDefined();
  });
});

test('the login page has no axe violation, no CSP violation, and no foreign request', async ({ page, baseURL }) => {
  await recordCspViolations(page);
  const foreign = recordForeignRequests(page, baseURL ?? '');
  await page.goto('/login');
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
  expect(await cspViolations(page)).toEqual([]);
  await expectNoAxeViolations(page);
  expect(foreign).toEqual([]);
  // The field with the accessible name "Salasana" is the password field.
  await expect(page.getByLabel('Salasana', { exact: true })).toHaveAttribute('type', 'password');
  await expect(page.getByLabel('Salasana', { exact: true })).toHaveAttribute('autocomplete', 'current-password');
});

test('the keyboard alone logs in and logs out', async ({ page, browserName }) => {
  // WebKit on macOS moves the focus with Tab only between text fields, as
  // Safari does by default; Option+Tab moves it to every element.
  const tab = browserName === 'webkit' && process.platform === 'darwin' ? 'Alt+Tab' : 'Tab';
  await page.goto('/login');
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();

  const field = passwordField(page);
  for (let presses = 0; presses < 10 && !(await isFocused(field)); presses += 1) {
    await page.keyboard.press(tab);
  }
  await expect(field).toBeFocused();
  await page.keyboard.type(TEST_PASSWORD);
  await page.keyboard.press('Enter');
  await expect(page.getByText('Palvelu toimii.')).toBeVisible();

  const logout = page.getByRole('button', { name: 'Kirjaudu ulos' });
  // The logout button is in the header, before the content. After the
  // login, the focus starts after the form, so move backwards: Firefox
  // does not wrap a forward Tab from the end to the start of the page.
  for (let presses = 0; presses < 10 && !(await isFocused(logout)); presses += 1) {
    await page.keyboard.press(`Shift+${tab}`);
  }
  await expect(logout).toBeFocused();
  await expectVisibleFocus(page);
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
});

async function isFocused(locator: ReturnType<Page['getByRole']>): Promise<boolean> {
  return locator.evaluate((element) => element === document.activeElement);
}

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
