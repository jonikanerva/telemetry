// The build of the SPA. The server serves web/dist: index.html with the
// version filled in, and the hashed files under /assets/ (STACK.md §15).
// The content security policy allows only same-origin scripts and styles,
// so the build must emit no inline script and no inline style.
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  base: '/',
  plugins: [react()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    assetsDir: 'assets',
    // The polyfill is an inline script; every supported browser has native
    // module preload.
    modulePreload: { polyfill: false },
    // Inline data URLs would hide assets inside the CSS and JS files.
    assetsInlineLimit: 0,
    sourcemap: false,
  },
});
