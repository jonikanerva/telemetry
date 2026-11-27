// The browser journeys run against the test server binary: the real router,
// middleware, and static files of the server, with the built SPA.
//
// Inputs, built before this command (mise task `test-web`):
// - target/debug/telemetry-web-server, from
//   `cargo build --locked -p telemetry-web-server --features test-server`
// - web/dist, from `pnpm run build:web`
//
// The server listens on 127.0.0.1 and serves HTTPS. The tests open
// https://localhost, because the `__Host-` session cookie needs the host
// name `localhost` and TLS in all three engines (e4-cookie.spec.ts).
// Before the server starts, e2e/tls-cert.sh makes a new self-signed
// certificate in e2e/.tls/ (ignored by Git). The browsers and the readiness
// check do not trust that certificate, so both ignore HTTPS errors:
// `use.ignoreHTTPSErrors` and `webServer.ignoreHTTPSErrors` of Playwright
// 1.65 (https://playwright.dev/docs/api/class-testoptions#test-options-ignore-https-errors,
// https://playwright.dev/docs/api/class-testconfig#test-config-web-server).
// The port is fixed, because every worker process loads this file and a
// random port would differ between the processes. E2E_PORT changes it.
// E2E_SERVER_BINARY replaces the path of the server binary, for a binary
// that another target directory holds.
//
// The session store, the login throttle, and the clock of the session
// rules are global in the one test server (ADR 0012). So one worker runs
// every test, and each test that logs in first calls the test-only route
// POST /test-only/auth/reset (auth-support.ts). Parallel workers would
// share the throttle bucket and the bound of 8 sessions, and a test could
// fail because of another test.
//
// The server checks the password against WEB_PASSWORD_HASH. The command
// makes the hash at each start with the `hash-password` command of the
// same binary, from the test-only password of auth-support.ts. So the
// repository holds no hash, and the journeys also check that the output of
// `hash-password` works for a login.
import path from 'node:path';
import process from 'node:process';

import { defineConfig, devices } from '@playwright/test';

import { TEST_PASSWORD } from './auth-support.js';
import { SERVER_VERSION } from './support.js';

const repositoryRoot = path.resolve(import.meta.dirname, '..');
const port = Number.parseInt(process.env['E2E_PORT'] ?? '4173', 10);
const baseURL = `https://localhost:${String(port)}`;
const tlsDir = path.join(repositoryRoot, 'e2e', '.tls');
const serverBinary =
  process.env['E2E_SERVER_BINARY'] ?? path.join(repositoryRoot, 'target', 'debug', 'telemetry-web-server');

export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.ts',
  outputDir: 'test-results',
  forbidOnly: process.env['CI'] === 'true',
  // A failure needs a diagnosis, not a second attempt (DOCTRINE.md P6).
  retries: 0,
  // One worker: the server state is global (see above).
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  use: {
    baseURL,
    ignoreHTTPSErrors: true,
    trace: 'retain-on-failure',
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
    { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
    { name: 'webkit', use: { ...devices['Desktop Safari'] } },
  ],
  webServer: {
    // Playwright runs the command in a shell. `exec` makes the server the
    // process that Playwright stops.
    // `printf '%s'` sends the password without a line end; the command
    // accepts both forms. A failure of `hash-password` leaves the variable
    // without a valid hash, and the server then stops at its start check.
    command: [
      `sh e2e/tls-cert.sh '${tlsDir}'`,
      `WEB_PASSWORD_HASH="$(printf '%s' "$E2E_TEST_PASSWORD" | '${serverBinary}' hash-password)"`,
      'export WEB_PASSWORD_HASH',
      'unset E2E_TEST_PASSWORD',
      `exec '${serverBinary}' serve-test`,
    ].join(' && '),
    cwd: repositoryRoot,
    env: {
      PORT: String(port),
      WEB_STATIC_DIR: path.join(repositoryRoot, 'web', 'dist'),
      RAILWAY_GIT_COMMIT_SHA: SERVER_VERSION,
      WEB_TEST_TLS_CERT: path.join(tlsDir, 'cert.pem'),
      WEB_TEST_TLS_KEY: path.join(tlsDir, 'key.pem'),
      WEB_PUBLIC_ORIGIN: new URL(baseURL).origin,
      E2E_TEST_PASSWORD: TEST_PASSWORD,
    },
    url: `${baseURL}/health`,
    ignoreHTTPSErrors: true,
    reuseExistingServer: false,
    timeout: 30_000,
    stdout: 'pipe',
    stderr: 'pipe',
  },
});
