// The journeys of the consumption view against the test server and the
// synthetic scenario files of e2e/scenarios: one for the data state, with
// the axe check, and one for a failure (503). The view model tests
// (web/src/consumption/view-state.test.ts) cover each state.
//
// The scenario `consumption-data` holds four drives (Helsinki time):
// 24.11. 08.00, 20 mi, 6,5 kWh; 25.11. 17.00, 12,5 mi, 4 kWh; 26.11.
// 11.00, 8 mi without EnergyRemaining; and today 09.30, 30 mi, 8,4 kWh,
// with an invalid Gear reading inside it.
import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';

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

test('data: the drives, the totals, and the charts', async ({ page, baseURL }) => {
  const foreign = recordForeignRequests(page, baseURL ?? '');
  await setScenario(page, baseURL, 'consumption-data');
  await page.goto('/battery?range=7d');
  await page.getByRole('navigation', { name: 'Näkymät' }).getByRole('link', { name: 'Kulutus' }).click();
  await expect(page).toHaveURL(/\/consumption\?range=7d$/);
  const view = page.locator('section.consumption');
  await expect(view).toHaveAttribute('data-state', 'data');
  await expect(page.getByTestId('consumption-data')).toHaveAttribute('data-partial', 'true');

  // The sums cover the three drives with a Wh/km value: 62,5 mi =
  // 100,6 km and 18,9 kWh; 18 900 Wh / 100,584 km = 187,9 Wh/km.
  await expect(page.getByTestId('totals')).toContainText('3 ajoa: 100,6 km, 18,9 kWh, 188 Wh/km.');
  await expect(page.getByTestId('totals')).toContainText('1 ajolla ei ole kulutusarvoa');
  // The recorded loss on 25.11. marks that drive; the car is asleep.
  await expect(page.getByTestId('totals')).toContainText('1 ajo on merkitty: datan menetys ajon aikana');
  await expect(page.getByTestId('status-line')).toHaveText(
    'Ei yhteyttä tänään klo 10.20 alkaen: auto nukkuu tai on verkon ulkopuolella. Uusin data tänään klo 10.15.',
  );

  const rows = page.getByTestId('drives-table').locator('tbody tr');
  await expect(rows).toHaveCount(4);
  // Newest first. 30 mi = 48,3 km; 8 400 Wh / 48,28 km = 174 Wh/km.
  await expect(rows.nth(0)).toContainText('tänään klo 09.30');
  await expect(rows.nth(0)).toContainText('48,3');
  await expect(rows.nth(0)).toContainText('174');
  await expect(rows.nth(1)).toContainText('ei tietoa');
  await expect(rows.nth(2)).toContainText('25.11.2026 klo 17.00 (datan menetys ajon aikana)');

  const byTemp = page.getByRole('figure', { name: 'Kulutus ja ulkolämpötila' });
  await expect(byTemp.getByTestId('scatter-point')).toHaveCount(3);
  await expect(byTemp.getByTestId('scatter-summary')).toHaveText('3/4 ajoa kaaviossa.');
  // The keyboard cursor reads the coldest drive first.
  await byTemp.getByRole('slider').focus();
  await expect(byTemp.getByTestId('scatter-readout')).toContainText('°C');

  expect(await cspViolations(page)).toEqual([]);
  const result = await new AxeBuilder({ page }).withTags(WCAG_AA_TAGS).analyze();
  expect(result.violations).toEqual([]);
  expect(foreign).toEqual([]);
});

test('failure: a 503 is unavailable, never an empty list', async ({ page, baseURL }) => {
  await setScenario(page, baseURL, 'fault-database-unavailable');
  await page.goto('/consumption?range=30d');
  const view = page.locator('section.consumption');
  await expect(view).toHaveAttribute('data-state', 'unavailable', { timeout: 10_000 });
  await expect(page.getByTestId('failure')).toContainText('Tietokanta ei ole juuri nyt käytettävissä');
  await expect(page.getByTestId('no-drives')).toHaveCount(0);
  await expect(page.getByTestId('drives-table')).toHaveCount(0);

  // The same view without drives says so.
  await setScenario(page, baseURL, 'consumption-no-drives');
  await page.getByRole('button', { name: 'Päivitä' }).click();
  await expect(view).toHaveAttribute('data-state', 'no_drives');
  await expect(page.getByTestId('no-drives')).toHaveText('Ei ajoja valitulla aikavälillä.');
  expect(await cspViolations(page)).toEqual([]);
});
