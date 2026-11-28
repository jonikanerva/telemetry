// The journeys of the battery view against the test server and the
// scenario files of e2e/scenarios (synthetic data only): one for the data
// state and one for a failure (503). The view model tests
// (web/src/battery/view-state.test.ts) cover each state of VISION.md.
//
// The browser runs in the zone America/New_York with the locale en-US, so
// a Helsinki time on the page proves that the page does not use the zone
// of the host. The query client retries a read once after 2 s for a 503
// (query-client.ts), so the failure journey sees two requests.
import AxeBuilder from '@axe-core/playwright';
import type { Page, Request } from '@playwright/test';

import { loginByApi, resetAuth } from './auth-support.js';
import { WCAG_AA_TAGS, cspViolations, expect, recordCspViolations, recordForeignRequests, test } from './support.js';

test.use({ timezoneId: 'America/New_York', locale: 'en-US' });

test.beforeEach(async ({ page, request, baseURL }) => {
  await resetAuth(request);
  await loginByApi(page, baseURL ?? '');
  await recordCspViolations(page);
});

async function setScenario(page: Page, baseURL: string | undefined, name: string): Promise<void> {
  await page.context().addCookies([{ name: 'telemetry_test_scenario', value: name, url: baseURL ?? '' }]);
}

function batteryRequests(page: Page): Request[] {
  const requests: Request[] = [];
  page.on('request', (request) => {
    if (new URL(request.url()).pathname === '/api/v1/battery') {
      requests.push(request);
    }
  });
  return requests;
}

test('data: status line, chart with a recorded loss and a restart, and the lists', async ({ page, baseURL }) => {
  const foreign = recordForeignRequests(page, baseURL ?? '');
  await setScenario(page, baseURL, 'data');
  // The start page opens the battery view with the default range.
  await page.goto('/');
  await expect(page).toHaveURL(/\/battery\?range=7d$/);
  const view = page.locator('section.battery');
  await expect(view).toHaveAttribute('data-state', 'data');

  // The fixed clock of the scenario is 12:00 UTC, 14.00 in Helsinki.
  await expect(page.getByTestId('fetched-at')).toHaveText('Haettu klo 14.00');
  await expect(page.getByTestId('status-line')).toHaveText('Yhdistetty tänään klo 13.10. Uusin data tänään klo 13.50.');

  // The recorded loss breaks the line and has a hatched area; the restart
  // has a tick. Sleep between the wake periods does not break the line.
  const soc = page.getByRole('figure', { name: 'Varaustaso' });
  await expect(soc.getByTestId('chart-line')).toHaveCount(2);
  await expect(soc.getByTestId('chart-gap')).toHaveCount(1);
  await expect(soc.getByTestId('chart-restart')).toHaveCount(1);
  await expect(soc.getByTestId('chart-summary')).toContainText('1 aukko valitulla välillä.');

  const gap = page.getByTestId('gap-row');
  await expect(gap).toHaveCount(1);
  await expect(gap).toContainText('eilen klo 11.00');
  await expect(gap).toContainText('Jono täyttyi, tietueita hylättiin');
  await expect(gap).toContainText('12 tietuetta');
  await expect(page.getByTestId('restart-item')).toHaveText(['25.11.2026 klo 13.49']);

  // The range lives in the URL, and Back returns.
  await page.getByRole('navigation', { name: 'Aikaväli' }).getByRole('link', { name: '30 pv' }).click();
  await expect(page).toHaveURL(/range=30d$/);
  await page.goBack();
  await expect(page).toHaveURL(/range=7d$/);
  await expect(view).toHaveAttribute('data-state', 'data');

  expect(await cspViolations(page)).toEqual([]);
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme });
    const result = await new AxeBuilder({ page }).withTags(WCAG_AA_TAGS).analyze();
    expect(result.violations, `axe violations in the ${colorScheme} color scheme`).toEqual([]);
  }
  expect(foreign).toEqual([]);
});

test('failure: a 503 is unavailable, never empty, and a failed refresh is announced', async ({ page, baseURL }) => {
  const requests = batteryRequests(page);
  await setScenario(page, baseURL, 'fault-database-unavailable');
  await page.goto('/battery?range=7d');
  const view = page.locator('section.battery');
  const status = page.getByTestId('fetched-at');
  await expect(view).toHaveAttribute('data-state', 'unavailable', { timeout: 10_000 });
  await expect(page.getByTestId('failure')).toContainText('Tietokanta ei ole juuri nyt käytettävissä');
  await expect(status).toHaveText('Tietoja ei saatu.');
  await expect(page.getByRole('figure')).toHaveCount(0);
  await expect(page.getByText('Ei dataa valitulla välillä.')).toHaveCount(0);
  expect(requests).toHaveLength(2);

  // A refresh with data, then a refresh that fails: the earlier data stays,
  // the one live region says so, and the focus stays on the button.
  const button = page.getByRole('button', { name: 'Päivitä' });
  await setScenario(page, baseURL, 'data');
  await button.focus();
  await page.keyboard.press('Enter');
  await expect(view).toHaveAttribute('data-state', 'data');
  await expect(status).toHaveText('Haettu klo 14.00');
  await expect(button).toBeFocused();

  await setScenario(page, baseURL, 'fault-database-unavailable');
  await page.keyboard.press('Enter');
  await expect(view).toHaveAttribute('data-state', 'stale_usable', { timeout: 10_000 });
  await expect(status).toHaveText('Päivitys epäonnistui. Näytetään aiemmin haettu data (haettu klo 14.00).');
  expect(await status.ariaSnapshot()).toBe(
    '- status: Päivitys epäonnistui. Näytetään aiemmin haettu data (haettu klo 14.00).',
  );
  await expect(button).toBeFocused();
  await expect(page.getByRole('figure', { name: 'Varaustaso' })).toBeVisible();
  expect(await cspViolations(page)).toEqual([]);
});
