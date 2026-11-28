// The unit tests of the API client facade and the SPA. The tests are pure
// functions and stub fetch functions, so they run in Node without a DOM.
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['packages/*/src/**/*.test.ts', 'web/src/**/*.test.ts'],
    environment: 'node',
    // Vitest replaces CSS with an empty module by default. The contrast test
    // of the design tokens (web/src/styles/tokens.test.ts) and the motion
    // test (web/src/styles/motion.test.ts) read the stylesheets.
    css: { include: [/\.css/] },
  },
});
