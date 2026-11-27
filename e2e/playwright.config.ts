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
import path from 'node:path';
import process from 'node:process';

import { defineConfig, devices } from '@playwright/test';

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
    command: `sh e2e/tls-cert.sh '${tlsDir}' && exec '${serverBinary}' serve-test`,
    cwd: repositoryRoot,
    env: {
      PORT: String(port),
      WEB_STATIC_DIR: path.join(repositoryRoot, 'web', 'dist'),
      RAILWAY_GIT_COMMIT_SHA: SERVER_VERSION,
      WEB_TEST_TLS_CERT: path.join(tlsDir, 'cert.pem'),
      WEB_TEST_TLS_KEY: path.join(tlsDir, 'key.pem'),
    },
    url: `${baseURL}/health`,
    ignoreHTTPSErrors: true,
    reuseExistingServer: false,
    timeout: 30_000,
    stdout: 'pipe',
    stderr: 'pipe',
  },
});
