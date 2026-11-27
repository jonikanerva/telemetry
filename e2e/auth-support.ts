// Helpers of the journeys that need a session (S2, and S4 for the battery
// view).
//
// The test server has one session store and one login throttle for all
// tests (playwright.config.ts runs one worker). Call `resetAuth` before a
// test that logs in, so the test starts with no session, a full throttle
// bucket, and the real clock.
import type { APIRequestContext, Page } from '@playwright/test';

import { expect } from './support.js';

/**
 * The password of the test server. It is a test-only value: the test
 * server hashes it at each start (playwright.config.ts). It is never a
 * real password.
 */
export const TEST_PASSWORD = 'e2e test-only password, not a secret 0001';

/** The name of the session cookie (ADR 0012). */
export const SESSION_COOKIE = '__Host-telemetry_session';

/** Ends every session, fills the throttle bucket, and resets the clock. */
export async function resetAuth(request: APIRequestContext): Promise<void> {
  const response = await request.post('/test-only/auth/reset');
  expect(response.status()).toBe(204);
}

/** Moves the clock of the session rules forward by `seconds`. */
export async function advanceClock(request: APIRequestContext, seconds: number): Promise<void> {
  const response = await request.post(`/test-only/auth/advance?seconds=${String(seconds)}`);
  expect(response.status()).toBe(204);
}

/**
 * Logs in through the API with the cookie store of the page, without the
 * login form. Use it in journeys that test another view.
 */
export async function loginByApi(page: Page, baseURL: string): Promise<void> {
  const response = await page.request.post('/api/v1/session', {
    data: { password: TEST_PASSWORD },
    headers: { origin: new URL(baseURL).origin },
  });
  expect(response.status()).toBe(200);
}

/** The counts of the test server since the last reset. */
export type AuthStats = { readonly refused_origin: number; readonly verifications: number };

export async function authStats(request: APIRequestContext): Promise<AuthStats> {
  const response = await request.get('/test-only/auth/stats');
  expect(response.status()).toBe(200);
  return (await response.json()) as AuthStats;
}
