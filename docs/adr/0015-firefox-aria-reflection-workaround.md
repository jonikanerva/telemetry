# ADR 0015: Firefox ARIA reflection workaround in the browser journeys

- Status: Accepted, 28 November 2026. A scoped exception under
  `DOCTRINE.md` "Exceptions". Decided by the lead within delegated
  authority, with approval by an independent reviewer (qa-enforcer) in PR
  #51: <https://github.com/jonikanerva/telemetry/pull/51#pullrequestreview-6093024203>,
  with the conditions of the addendum
  <https://github.com/jonikanerva/telemetry/pull/51#issuecomment-7227466133>.
- Responsible lead: the lead agent of the delivery task.
- Scope: the Playwright journeys of `e2e/` in the project `firefox` only,
  with Playwright 1.65.1 and its Firefox 159.0, build 1572. The workaround
  does not change the application, the release image, or the journeys in
  Chromium and WebKit.

## Context

- The Firefox of Playwright 1.65.1 (Firefox 159.0, build 1572) crashes its
  content process (SIGSEGV in
  `Element::GetAttrAssociatedElementsForBindings`) when two JavaScript
  worlds read `ariaLabelledByElements` of one element:
  - Playwright's utility world reads the property in the accessible name
    computation of a role query with a name, of `ariaSnapshot()`, and of a
    polling `expect`.
  - The page's main world reads the property in axe-core 4.14.1 (rule data
    `prop: 'ariaLabelledByElements'`, function `getIdrefsValue`) and in
    `page.evaluate`.
- Playwright then reports "Page crashed". A minimal page reproduces the
  crash without the application:
  `<section aria-labelledby="t"><h1 id="t">x</h1></section>`, a
  `getByRole('region', { name: 'x' })` query, then a main-world read of
  the property.
- The crash is not specific to macOS. CI on linux/amd64 crashed six
  Firefox journeys of the battery-view slice:
  <https://github.com/jonikanerva/telemetry/actions/runs/46054099506/job/140692284877>.
- Upstream records:
  - Mozilla bug 2090055 (RESOLVED FIXED, Firefox 163):
    <https://bugzilla.mozilla.org/show_bug.cgi?id=2090055>.
  - Playwright issues 43967 and 44135 (closed):
    <https://github.com/microsoft/playwright/issues/43967>,
    <https://github.com/microsoft/playwright/issues/44135>.
  - Playwright PR 43982 rolls Firefox to build 1575 (merged):
    <https://github.com/microsoft/playwright/pull/43982>.
  - axe-core issue 5549 (open):
    <https://github.com/dequelabs/axe-core/issues/5549>.
- On 28 November 2026, the npm tag `latest` of `@playwright/test` is
  1.65.1. The tag `next` is 1.66.0-alpha. No stable release ships Firefox
  build 1575 or later.
- The Playwright 1.65.1 utility world reads only `ariaLabelledByElements`
  of the eight ARIA element-reflection properties of WAI-ARIA 1.3
  (`ariaActiveDescendantElement`, `ariaControlsElements`,
  `ariaDescribedByElements`, `ariaDetailsElements`,
  `ariaErrorMessageElements`, `ariaFlowToElements`,
  `ariaLabelledByElements`, `ariaOwnsElements`). A reproduction with each
  property, a role query, a description query, an error message query,
  an `ariaSnapshot()`, and a main-world read crashed only for
  `ariaLabelledByElements`. The crash of the other seven properties is
  not reachable from the utility world of Playwright 1.65.1.
- The same reproduction with the Firefox build 1584 (Firefox 160.0),
  driven by Playwright 1.65.1 on macOS arm64, did not crash. Build 1575
  was not tested.
- The application on `main` passes the Firefox journeys without the
  workaround. The crash comes when a journey queries a name and then runs
  axe on an element with `aria-labelledby`.

## Decision

1. In the Playwright project `firefox` only, the shared fixture of
   `e2e/support.ts` adds an init script to each browser context. The init
   script deletes `ariaLabelledByElements` from `Element.prototype` in the
   main world, before any page script runs. axe-core then reads
   `aria-labelledby` from the content attribute. The list of deleted
   properties is `HIDDEN_ARIA_REFLECTION_PROPERTIES` in `e2e/support.ts`.
2. `e2e/support.ts` is the only e2e file that imports values from
   `@playwright/test`, `playwright`, `playwright/test`, or
   `playwright-core`. Every other file under `e2e/` imports types only
   from these modules, and takes `test`, `expect`, and the helpers from
   `e2e/support.ts`. The lint rule `no-restricted-imports` for `e2e/**` in
   `.oxlintrc.json` enforces this, also for a default import, a re-export,
   a spec in a subdirectory, and a helper file. `e2e/playwright.config.ts`
   is the second exempt file, because it needs `defineConfig` and
   `devices`.
3. A test that needs a further browser context opens it with
   `newBrowserContext` of `e2e/support.ts`. In Firefox, that context gets
   the same init script. The lint rule `no-restricted-properties` for
   `e2e/**` fails on `newContext`, `launch`, `launchPersistentContext`,
   `launchServer`, `connect`, and `connectOverCDP` of any object, and on
   `browser.newPage`, outside `e2e/support.ts`. A page of a context from
   the fixture or from `newBrowserContext` gets the init script.
4. Each slice that adds a page or a state with an axe check adds the page
   or state to the equivalence test.
5. Chromium and WebKit run unchanged. The journeys use no retry, skip no
   engine, and disable no axe rule.

## Compensating evidence

`e2e/firefox-aria-reflection.spec.ts` holds these tests. Each test can
fail:

- Regression (Firefox): a role query with a name, a main-world read of
  the property, axe, and a second role query on a page with
  `aria-labelledby` do not crash the page. Without the fixture, the test
  fails with "Page crashed".
- Challenge (Firefox, with the fixture): axe reports a `button-name`
  violation for a button whose `aria-labelledby` names a missing id, and a
  pass for a button with a resolved reference.
- Context (Firefox): a second page of the fixture context and a page of a
  context from `newBrowserContext` do not have the property in the main
  world.
- Equivalence (Chromium and WebKit): on each page and state that a
  journey checks with axe, and on the not-found page, in both colour
  schemes, the axe violations, passes, and incomplete results are equal
  with and without the deletion. The test checks that the property is
  present in the control and absent with the deletion. Each case states
  whether its page has an element with `aria-labelledby`, and the test
  checks that statement; for a page with such an element, it checks that
  axe checked a node with `aria-labelledby`.
- Source check (Firefox project, no browser): the application source in
  `web/src` and `packages/api-client/src`, without `generated/`, does not
  name a property of `HIDDEN_ARIA_REFLECTION_PROPERTIES`.
- Removal guard (Firefox project, no browser): the test fails when the
  Firefox revision in `browsers.json` of the pinned `playwright-core` is
  1575 or later. The message tells the reader to remove the workaround
  and this exception.

The lint rules of decisions 2 and 3 support these tests: they keep the
fixture in front of every page. Their known limits:

- They do not find a dynamic `import()` or a `require()` of a Playwright
  module.
- The rule for `newPage` names the object `browser` only. A `Browser`
  under another variable name can call `newPage` without a finding.
  `context.newPage()` stays allowed, because the context comes from the
  fixture or from `newBrowserContext`.

## Key alternatives

- Wait for a stable Playwright release with Firefox build 1575 or later.
  Rejected: no such release exists, and the open slices need green
  Firefox journeys now.
- Adopt Playwright 1.66 alpha. Rejected: a prerelease needs a reviewed
  exception of its own (`DOCTRINE.md` P7), and it changes every browser.
- Downgrade Playwright. Rejected: P7 requires the latest stable release.
- Change the markup (no `aria-labelledby`). Rejected: the accessible name
  from the visible heading is the better markup, and a test tool must not
  decide the markup.
- Run axe only in Chromium and WebKit. Rejected: it removes the axe check
  of one engine completely.
- Exclude the axe rules that read `aria-labelledby`. Rejected: it removes
  those checks in every engine.
- Retry the failed journeys. Rejected: a retry hides a failure
  (`DOCTRINE.md` P6), and the crash is deterministic.

## Consequences

- In Firefox, axe does not check a reference that a script sets only
  through the property `ariaLabelledByElements`, with no content
  attribute. The application sets no such reference; the source check
  enforces this. Chromium and WebKit still check such a reference.
- A context that a test opens with `newBrowserContext` gets the
  workaround. A context that bypasses the helper through a gap of the lint
  rules does not.
- The journeys do not show whether a stock Firefox 159 crashes for a user,
  for example with a browser extension that reads the property in a
  second world. This is not verified.

## Cost of reversal

Low. Remove the fixture override and the constants from `e2e/support.ts`,
the spec `e2e/firefox-aria-reflection.spec.ts`, the two lint overrides
for `e2e/` in `.oxlintrc.json` if no other reason keeps them, and the row in `STACK.md`
§13. Then set the status of this ADR to "Superseded" or "Expired".

## Reassessment

Remove the workaround when a stable Playwright release with Firefox build
1575 or later is older than 1440 minutes, or on 2027-02-28, whichever
comes first. The removal guard fails when the pinned Playwright ships
such a build.
