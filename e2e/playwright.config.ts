// The browser journeys run against the test server binary: the real router,
// middleware, and static files of the server, with the built SPA.
//
// Inputs, built before this command (mise task `test-web`):
// - target/debug/telemetry-web-server, from
//   `cargo build --locked -p telemetry-web-server --features test-server`
// - web/dist, from `pnpm run build:web`
//
// The server listens on 127.0.0.1. The tests open http://localhost, because
// experiment E4 needs the host name `localhost` (e4-cookie.spec.ts).
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
const baseURL = `http://localhost:${String(port)}`;
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
    trace: 'retain-on-failure',
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
    { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
    { name: 'webkit', use: { ...devices['Desktop Safari'] } },
  ],
  webServer: {
    command: `${serverBinary} serve-test`,
    cwd: repositoryRoot,
    env: {
      PORT: String(port),
      WEB_STATIC_DIR: path.join(repositoryRoot, 'web', 'dist'),
      RAILWAY_GIT_COMMIT_SHA: SERVER_VERSION,
    },
    url: `${baseURL}/health`,
    reuseExistingServer: false,
    timeout: 30_000,
    stdout: 'pipe',
    stderr: 'pipe',
  },
});
