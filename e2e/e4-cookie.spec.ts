// Experiment E4: each browser engine keeps a `__Host-` cookie with the
// Secure attribute on the test server. The session cookie of the login
// (S2) depends on it.
//
// Over plain http://localhost, the webkit engine drops the cookie, so the
// test server serves HTTPS (playwright.config.ts). Every engine must keep
// the cookie over https://localhost.
//
// The requests run inside the page with fetch, so the cookie store of the
// engine decides. The `request` fixture of Playwright has its own cookie
// store and would give the same answer in every engine.
import { expect, test } from '@playwright/test';

type Outcome = { readonly setStatus: number; readonly checkStatus: number; readonly body: unknown };

test('E4: a __Host- Secure cookie over https://localhost', async ({ page, browserName }) => {
  await page.goto('/');
  const url = new URL(page.url());
  expect(url.protocol).toBe('https:');
  expect(url.hostname).toBe('localhost');

  const outcome = await page.evaluate(async (): Promise<Outcome> => {
    const set = await fetch('/test-only/e4/cookie/set', { credentials: 'same-origin' });
    const check = await fetch('/test-only/e4/cookie/check', { credentials: 'same-origin' });
    return { setStatus: set.status, checkStatus: check.status, body: (await check.json()) as unknown };
  });

  expect(outcome.setStatus).toBe(204);
  expect(outcome.checkStatus).toBe(200);
  const body = outcome.body;
  if (typeof body !== 'object' || body === null || !('present' in body) || typeof body.present !== 'boolean') {
    throw new Error('the check route returned no {"present": boolean} body');
  }
  console.log(`E4 ${browserName}: present=${String(body.present)}`);
  expect(body.present, `${browserName} must keep the __Host- cookie over HTTPS`).toBe(true);
});
