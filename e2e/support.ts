// Shared helpers of the browser journeys.
import type { Page } from '@playwright/test';

export { expect, test } from '@playwright/test';

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
