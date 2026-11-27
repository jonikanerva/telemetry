// The unit tests of the API client facade and the SPA. The tests are pure
// functions and stub fetch functions, so they run in Node without a DOM.
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['packages/*/src/**/*.test.ts', 'web/src/**/*.test.ts'],
    environment: 'node',
  },
});
