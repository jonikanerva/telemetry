// Shared fixture and helpers of the browser journeys. This file is the
// only e2e file that imports values from Playwright; every other e2e file
// imports types only and takes `test`, `expect`, and the context helpers
// from here. It is also the only e2e file that opens a browser context or
// an API request context itself. The lint rules no-restricted-imports and
// no-restricted-properties for `e2e/**` in .oxlintrc.json enforce both.
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';

import { test as base, expect, firefox } from '@playwright/test';
import type {
  APIRequest,
  APIRequestContext,
  Browser,
  BrowserContext,
  BrowserContextOptions,
  Page,
} from '@playwright/test';

export { expect };

/** The version that the test server reports, in /health and in index.html. */
export const SERVER_VERSION = '0123abc';

/** The axe rule tags of WCAG 2.2 level AA and the levels below it. */
export const WCAG_AA_TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'];

declare global {
  interface Window {
    cspViolations?: string[];
  }
}

/**
 * Records every content security policy violation of the page. The script
 * runs before any page script, so it also sees violations during the load.
 */
export async function recordCspViolations(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const violations: string[] = [];
    window.cspViolations = violations;
    document.addEventListener('securitypolicyviolation', (event) => {
      violations.push(`${event.violatedDirective} ${event.blockedURI}`);
    });
  });
}

export async function cspViolations(page: Page): Promise<string[]> {
  return page.evaluate(() => window.cspViolations ?? ['the recorder did not run']);
}

/** Records the URL of every request that does not go to the test server. */
export function recordForeignRequests(page: Page, baseURL: string): string[] {
  const origin = new URL(baseURL).origin;
  const foreign: string[] = [];
  page.on('request', (request) => {
    const url = new URL(request.url());
    // The empty favicon of index.html is a data URL; it leaves the browser
    // with no request.
    if (url.protocol !== 'data:' && url.origin !== origin) {
      foreign.push(url.origin);
    }
  });
  return foreign;
}

/**
 * The ARIA element-reflection properties that the Firefox workaround
 * deletes from `Element.prototype` (ADR 0015). The Firefox of Playwright
 * 1.65.1 (Firefox 159.0, build 1572) crashes its content process when
 * Playwright's utility world and the page's main world both read
 * `ariaLabelledByElements` of one element (Mozilla bug 2090055). The
 * utility world reads no other reflection property, so the list holds
 * only this one.
 */
export const HIDDEN_ARIA_REFLECTION_PROPERTIES: readonly string[] = ['ariaLabelledByElements'];

/**
 * The first Firefox build of Playwright that has the fix of Mozilla bug
 * 2090055 (Playwright PR 43982). The removal guard in
 * firefox-aria-reflection.spec.ts fails from this build on.
 */
export const FIXED_FIREFOX_BUILD = 1575;

/**
 * Deletes the properties of HIDDEN_ARIA_REFLECTION_PROPERTIES in the main
 * world of each page of the context, before any page script runs. axe-core
 * then reads `aria-labelledby` from the content attribute. Playwright's
 * utility world is a separate JavaScript world; its role queries still
 * resolve a name from `aria-labelledby` (firefox-aria-reflection.spec.ts).
 */
export async function hideAriaReflection(context: BrowserContext): Promise<void> {
  await context.addInitScript((names: readonly string[]) => {
    for (const name of names) {
      Reflect.deleteProperty(Element.prototype, name);
    }
  }, HIDDEN_ARIA_REFLECTION_PROPERTIES);
}

/**
 * The test function of every journey. In the Firefox project only, the
 * context fixture hides the ARIA element reflection of
 * HIDDEN_ARIA_REFLECTION_PROPERTIES, so that axe-core in the main world
 * and the role queries of Playwright do not crash the content process.
 * Chromium and WebKit run unchanged. The workaround is a scoped exception
 * (ADR 0015, STACK.md §13). Remove it when the pinned Playwright ships
 * Firefox build FIXED_FIREFOX_BUILD or later; the removal guard fails then.
 * A test that needs a further browser context opens it with
 * `newBrowserContext`, which applies the same workaround.
 */
export const test = base.extend({
  // The second argument is Playwright's `use`, named `provide` so that the
  // React hook rule does not read it as a hook call.
  context: async ({ context, browserName }, provide) => {
    if (browserName === 'firefox') {
      await hideAriaReflection(context);
    }
    await provide(context);
  },
});

/**
 * Opens a new browser context. In Firefox, the context gets the workaround
 * of the `context` fixture. Use it instead of `browser.newContext()`.
 */
export async function newBrowserContext(browser: Browser, options: BrowserContextOptions): Promise<BrowserContext> {
  const context = await browser.newContext(options);
  if (browser.browserType().name() === 'firefox') {
    await hideAriaReflection(context);
  }
  return context;
}

/**
 * Opens a new API request context. An API request context has no page, so
 * the workaround does not apply; the helper exists so that only this file
 * calls `newContext`.
 */
export async function newRequestContext(
  request: APIRequest,
  options: Parameters<APIRequest['newContext']>[0],
): Promise<APIRequestContext> {
  return request.newContext(options);
}

/** The path of the Firefox executable that the tests launch. */
export function firefoxExecutablePath(): string {
  return firefox.executablePath();
}

/** The Firefox build that browsers.json of the pinned playwright-core names. */
export function pinnedFirefoxBuild(): number {
  // playwright-core is a dependency of playwright, which is a dependency of
  // @playwright/test; pnpm links only the direct dependencies.
  const fromTest = createRequire(createRequire(import.meta.url).resolve('@playwright/test/package.json'));
  const fromPlaywright = createRequire(fromTest.resolve('playwright/package.json'));
  const corePackage = fromPlaywright.resolve('playwright-core/package.json');
  const parsed: unknown = JSON.parse(readFileSync(path.join(path.dirname(corePackage), 'browsers.json'), 'utf8'));
  if (typeof parsed !== 'object' || parsed === null || !('browsers' in parsed) || !Array.isArray(parsed.browsers)) {
    throw new Error('browsers.json of playwright-core has no browsers list');
  }
  const entries: unknown[] = parsed.browsers;
  const entry = entries.find(
    (candidate): candidate is { name: string; revision: string } =>
      typeof candidate === 'object' &&
      candidate !== null &&
      'name' in candidate &&
      candidate.name === 'firefox' &&
      'revision' in candidate &&
      typeof candidate.revision === 'string',
  );
  if (entry === undefined || !/^\d+$/.test(entry.revision)) {
    throw new Error('browsers.json of playwright-core has no Firefox entry with a numeric revision');
  }
  return Number.parseInt(entry.revision, 10);
}
