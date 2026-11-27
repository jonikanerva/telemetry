// Compensating evidence of the Firefox ARIA reflection workaround of
// support.ts (ADR 0015, STACK.md §13):
// - the regression test: the two-world read does not crash Firefox;
// - the challenge test: axe in Firefox still checks `aria-labelledby`;
// - the context test: every page of a context of the fixture or of
//   newBrowserContext gets the workaround;
// - the equivalence test: in Chromium and WebKit, the deletion does not
//   change the axe result of any page and state with an axe check;
// - the source check: the application uses no hidden property;
// - the removal guard: it fails when the pinned Playwright ships a Firefox
//   build with the fix.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

import AxeBuilder from '@axe-core/playwright';
import type { APIRequestContext, Browser, Page } from '@playwright/test';

import { TEST_PASSWORD, loginByApi, resetAuth } from './auth-support.js';
import {
  FIXED_FIREFOX_BUILD,
  HIDDEN_ARIA_REFLECTION_PROPERTIES,
  WCAG_AA_TAGS,
  expect,
  firefoxExecutablePath,
  hideAriaReflection,
  newBrowserContext,
  pinnedFirefoxBuild,
  test,
} from './support.js';

const repositoryRoot = path.resolve(import.meta.dirname, '..');

/** A page that the test serves with `page.route`; no request reaches the server. */
async function openRoutedPage(page: Page, body: string): Promise<void> {
  await page.route('**/aria-reflection-fixture', (route) =>
    route.fulfill({ status: 200, contentType: 'text/html', body }),
  );
  await page.goto('/aria-reflection-fixture');
}

/** True when the main world of the page has the property on Element.prototype. */
async function hasReflection(page: Page, name: string): Promise<boolean> {
  return page.evaluate((property) => property in Element.prototype, name);
}

test.describe('Firefox with the workaround', () => {
  test.skip(({ browserName }) => browserName !== 'firefox', 'The workaround is active in Firefox only.');

  test('a role query and a main-world read of ariaLabelledByElements do not crash the page', async ({ page }) => {
    let crashed = false;
    page.on('crash', () => {
      crashed = true;
    });
    await openRoutedPage(
      page,
      '<!doctype html><html lang="en"><head><title>t</title></head><body><main>' +
        '<section aria-labelledby="t"><h1 id="t">Otsikko</h1></section></main></body></html>',
    );
    for (const name of HIDDEN_ARIA_REFLECTION_PROPERTIES) {
      // Soft, so that a missing workaround also shows the crash below.
      expect.soft(await hasReflection(page, name), `${name} must be hidden in the main world`).toBe(false);
    }

    // 1. The utility world of Playwright computes the name from aria-labelledby.
    await expect(page.getByRole('region', { name: 'Otsikko' })).toHaveCount(1);
    // 2. The main world reads the same reflected property, as axe-core does.
    const mainWorld = await page.evaluate(() => {
      const section = document.querySelector('section');
      if (section === null) {
        return 'no section';
      }
      const value: unknown = Reflect.get(section, 'ariaLabelledByElements');
      return typeof value;
    });
    expect(mainWorld).toBe('undefined');
    // 3. axe-core and a further role query on the same element.
    const result = await new AxeBuilder({ page }).withTags(WCAG_AA_TAGS).analyze();
    expect(result.violations).toEqual([]);
    await expect(page.getByRole('region', { name: 'Otsikko' })).toHaveCount(1);
    expect(crashed).toBe(false);
  });

  test('axe reports an aria-labelledby reference to a missing id and passes a resolved one', async ({ page }) => {
    await openRoutedPage(
      page,
      '<!doctype html><html lang="en"><head><title>t</title></head><body><main><h1>h</h1>' +
        '<span id="label">Tallenna</span>' +
        '<button id="resolved" aria-labelledby="label"></button>' +
        '<button id="missing" aria-labelledby="no-such-id"></button></main></body></html>',
    );
    expect(await hasReflection(page, 'ariaLabelledByElements')).toBe(false);
    // The utility world still resolves the name of the resolved reference.
    await expect(page.getByRole('button', { name: 'Tallenna' })).toHaveCount(1);

    const result = await new AxeBuilder({ page }).withRules(['button-name']).analyze();
    const targets = (kind: 'violations' | 'passes'): string[] =>
      result[kind].flatMap((rule) => rule.nodes.map((node) => node.target.join(' ')));
    expect(targets('violations')).toEqual(['#missing']);
    expect(targets('passes')).toEqual(['#resolved']);
  });

  test('a second page of the context and a context of newBrowserContext also hide the property', async ({
    context,
    browser,
    baseURL,
  }) => {
    const second = await context.newPage();
    await openRoutedPage(second, '<!doctype html><title>t</title>');
    expect(await hasReflection(second, 'ariaLabelledByElements')).toBe(false);

    if (baseURL === undefined) {
      throw new Error('the project has no baseURL');
    }
    const other = await newBrowserContext(browser, { baseURL, ignoreHTTPSErrors: true });
    try {
      const page = await other.newPage();
      await openRoutedPage(page, '<!doctype html><title>t</title>');
      expect(await hasReflection(page, 'ariaLabelledByElements')).toBe(false);
    } finally {
      await other.close();
    }
  });

  test('the application source uses no hidden reflection property', () => {
    const roots = [path.join(repositoryRoot, 'web', 'src'), path.join(repositoryRoot, 'packages', 'api-client', 'src')];
    const excluded = path.join(repositoryRoot, 'packages', 'api-client', 'src', 'generated');
    const findings = roots.flatMap((root) => findProperties(root, excluded, HIDDEN_ARIA_REFLECTION_PROPERTIES));
    expect(
      findings,
      'The Firefox workaround hides these properties in the journeys, so the journeys cannot test their use (ADR 0015).',
    ).toEqual([]);
  });

  test('the pinned Playwright has a Firefox build without the fix', () => {
    const build = pinnedFirefoxBuild();
    // The browser that the tests launch has the same build.
    expect(firefoxExecutablePath()).toContain(`firefox-${String(build)}`);
    expect(
      build,
      `The pinned Playwright ships Firefox build ${String(build)}, which has the fix of Mozilla bug 2090055. ` +
        'Remove the workaround of e2e/support.ts, this spec, the exception row of STACK.md §13, and close ADR 0015.',
    ).toBeLessThan(FIXED_FIREFOX_BUILD);
  });
});

test.describe('Chromium and WebKit', () => {
  test.skip(({ browserName }) => browserName === 'firefox', 'The control run without the workaround crashes Firefox.');

  type Case = {
    readonly name: string;
    /** Whether the page of the case has an element with `aria-labelledby`. */
    readonly labelledBy: boolean;
    /** Brings a new page of a fresh context into the state of the case. */
    readonly open: (page: Page, baseURL: string) => Promise<void>;
  };

  async function startPage(page: Page, baseURL: string, health?: Parameters<typeof fulfillHealth>[1]): Promise<void> {
    await loginByApi(page, baseURL);
    if (health !== undefined) {
      await fulfillHealth(page, health);
    }
    await page.goto('/');
  }

  async function fulfillHealth(
    page: Page,
    health: { readonly status: number; readonly contentType: string; readonly body: string },
  ): Promise<void> {
    await page.route('**/health', (route) => route.fulfill(health));
  }

  async function submitPassword(page: Page, password: string): Promise<void> {
    await page.getByLabel('Salasana', { exact: true }).fill(password);
    await page.getByRole('button', { name: 'Kirjaudu sisään' }).click();
  }

  // Each page and state on main that a journey checks with axe
  // (smoke.spec.ts, login.spec.ts), and the not-found page. A slice that
  // adds a page or a state with an axe check adds it here (ADR 0015).
  const cases: readonly Case[] = [
    {
      name: 'protected start page',
      labelledBy: false,
      open: async (page, baseURL) => {
        await startPage(page, baseURL);
        await expect(page.getByText('Palvelu toimii.')).toBeVisible();
      },
    },
    {
      name: 'start page, unavailable state',
      labelledBy: false,
      open: async (page, baseURL) => {
        await startPage(page, baseURL, { status: 502, contentType: 'text/html', body: '<html>Bad Gateway</html>' });
        await expect(page.getByText('Tietoja ei saatu')).toBeVisible();
      },
    },
    {
      name: 'start page, reload banner',
      labelledBy: false,
      open: async (page, baseURL) => {
        await startPage(page, baseURL, {
          status: 200,
          contentType: 'application/json',
          body: '{"status":"ok","version":"fedcba9"}',
        });
        await expect(page.getByText('Sovelluksesta on uusi versio.')).toBeVisible();
      },
    },
    {
      name: 'not-found page',
      labelledBy: true,
      open: async (page, baseURL) => {
        await loginByApi(page, baseURL);
        await page.goto('/no-such-page');
        await expect(page.getByRole('heading', { level: 1, name: 'Sivua ei löydy' })).toBeVisible();
      },
    },
    {
      name: 'login page',
      labelledBy: false,
      open: async (page) => {
        await page.goto('/login');
        await expect(page.getByRole('heading', { level: 1, name: 'Kirjaudu sisään' })).toBeVisible();
      },
    },
    {
      name: 'login page, wrong password',
      labelledBy: false,
      open: async (page) => {
        await page.goto('/login');
        await submitPassword(page, 'wrong password, test only');
        await expect(page.getByRole('alert')).toHaveText('Salasana on väärä.');
      },
    },
    {
      name: 'login page, throttled',
      labelledBy: false,
      open: async (page) => {
        await page.goto('/login');
        for (let attempt = 0; attempt < 5; attempt += 1) {
          await submitPassword(page, `wrong password ${String(attempt)}`);
          await expect(page.getByRole('alert')).toHaveText('Salasana on väärä.');
        }
        await submitPassword(page, TEST_PASSWORD);
        await expect(page.getByRole('alert')).toContainText('Liian monta kirjautumisyritystä.');
      },
    },
    {
      name: 'login page, server unavailable',
      labelledBy: false,
      open: async (page) => {
        await page.goto('/login');
        await page.route('**/api/v1/session', (route) =>
          route.request().method() === 'POST'
            ? route.fulfill({ status: 502, contentType: 'text/html', body: '<html>Bad Gateway</html>' })
            : route.fallback(),
        );
        await submitPassword(page, TEST_PASSWORD);
        await expect(page.getByRole('alert')).toContainText('Kirjautuminen ei onnistunut.');
      },
    },
    {
      name: 'logout failure view',
      labelledBy: false,
      open: async (page, baseURL) => {
        await startPage(page, baseURL);
        await expect(page.getByText('Palvelu toimii.')).toBeVisible();
        await page.route('**/api/v1/session', (route) =>
          route.request().method() === 'DELETE' ? route.abort('failed') : route.fallback(),
        );
        await page.getByRole('button', { name: 'Kirjaudu ulos' }).click();
        await expect(page.getByRole('heading', { level: 1, name: 'Uloskirjautuminen ei onnistunut' })).toBeVisible();
      },
    },
  ];

  for (const testCase of cases) {
    test(`the deletion does not change the axe result: ${testCase.name}`, async ({ browser, baseURL, request }) => {
      if (baseURL === undefined) {
        throw new Error('the project has no baseURL');
      }
      const control = await axeSummary(browser, baseURL, request, testCase, false);
      const treatment = await axeSummary(browser, baseURL, request, testCase, true);
      expect(control.reflection, 'the control has the property').toBe(true);
      expect(treatment.reflection, 'the deletion removes the property').toBe(false);
      expect(control.labelledByElements > 0, 'the page has an element with aria-labelledby').toBe(testCase.labelledBy);
      if (testCase.labelledBy) {
        expect(control.labelledByNodes, 'axe checked a node with aria-labelledby').toBeGreaterThan(0);
      }
      expect(treatment.result).toEqual(control.result);
    });
  }

  async function axeSummary(
    browser: Browser,
    baseURL: string,
    request: APIRequestContext,
    testCase: Case,
    deletion: boolean,
  ): Promise<{
    readonly reflection: boolean;
    readonly labelledByElements: number;
    readonly labelledByNodes: number;
    readonly result: string[];
  }> {
    // Each run starts with no session, a full throttle bucket, and the real clock.
    await resetAuth(request);
    const context = await newBrowserContext(browser, { baseURL, ignoreHTTPSErrors: true });
    try {
      if (deletion) {
        await hideAriaReflection(context);
      }
      const page = await context.newPage();
      await testCase.open(page, baseURL);
      const reflection = await hasReflection(page, 'ariaLabelledByElements');
      const labelledByElements = await page.locator('[aria-labelledby]').count();
      const result: string[] = [];
      let labelledByNodes = 0;
      for (const colorScheme of ['light', 'dark'] as const) {
        await page.emulateMedia({ colorScheme });
        const axe = await new AxeBuilder({ page }).withTags(WCAG_AA_TAGS).analyze();
        for (const kind of ['violations', 'passes', 'incomplete'] as const) {
          for (const rule of axe[kind]) {
            for (const node of rule.nodes) {
              if (node.html.includes('aria-labelledby')) {
                labelledByNodes += 1;
              }
              const checks = [...node.any, ...node.all, ...node.none].map((check) => `${check.id}:${check.message}`);
              result.push(
                `${colorScheme} ${kind} ${rule.id} ${node.target.join(' ')} ${String(node.impact)} ${checks.sort().join(',')}`,
              );
            }
          }
        }
      }
      return { reflection, labelledByElements, labelledByNodes, result: result.sort() };
    } finally {
      await context.close();
    }
  }
});

/** Each `file: property` pair where a file under `root` names a property. */
function findProperties(root: string, excluded: string, properties: readonly string[]): string[] {
  const findings: string[] = [];
  const visit = (directory: string): void => {
    for (const entry of readdirSync(directory)) {
      const file = path.join(directory, entry);
      if (file === excluded) {
        continue;
      }
      if (statSync(file).isDirectory()) {
        visit(file);
        continue;
      }
      const text = readFileSync(file, 'utf8');
      for (const property of properties) {
        if (text.includes(property)) {
          findings.push(`${path.relative(repositoryRoot, file)}: ${property}`);
        }
      }
    }
  };
  visit(root);
  return findings;
}
