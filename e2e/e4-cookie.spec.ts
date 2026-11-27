// Experiment E4: does each browser engine keep a `__Host-` cookie with the
// Secure attribute over http://localhost? The session cookie of the login
// (S2) depends on the answer for the local test server.
//
// The requests run inside the page with fetch, so the cookie store of the
// engine decides. The `request` fixture of Playwright has its own cookie
// store and would give the same answer in every engine.
//
// The test records the result of each engine as an annotation. It fails
// only when Chromium or Firefox does not return the cookie. The WebKit
// result is recorded for the lead and does not fail the test.
import { expect, test } from '@playwright/test';

type Outcome = { readonly setStatus: number; readonly checkStatus: number; readonly body: unknown };

test('E4: a __Host- Secure cookie over http://localhost', async ({ page, browserName }, testInfo) => {
  await page.goto('/');
  expect(new URL(page.url()).hostname).toBe('localhost');

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
  const present = body.present;

  testInfo.annotations.push({
    type: 'E4',
    description: `${browserName}: the __Host- Secure cookie ${present ? 'came back' : 'did not come back'} over http://localhost`,
  });
  console.log(`E4 ${browserName}: present=${String(present)}`);

  if (browserName !== 'webkit') {
    expect(present, `${browserName} must keep the cookie`).toBe(true);
  }
});
