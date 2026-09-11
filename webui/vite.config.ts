import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

/*
 * The appliance shell is published as a pair of assets with stable names, not
 * as a page: the page it attaches to is the media experience, composed at
 * deploy time. Stable names are what let that composition be a two-line edit
 * instead of a build-output parser.
 *
 * `base: './'` keeps every emitted reference relative, so the bundle works from
 * any mount point without being rewritten.
 */
export default defineConfig({
  base: './',
  plugins: [react()],
  build: {
    outDir: 'dist',
    sourcemap: false,
    rollupOptions: {
      output: {
        entryFileNames: 'mediabox/shell.js',
        chunkFileNames: 'mediabox/[name].js',
        assetFileNames: (asset) =>
          asset.names?.[0]?.endsWith('.css') ? 'mediabox/shell.css' : 'mediabox/[name][extname]',
      },
    },
  },
  server: {
    port: 5173,
    // Only used by `npm run dev`; production serves the API from the same origin.
    proxy: {
      '/api': { target: 'http://localhost:8787', changeOrigin: true, ws: true },
      '/server': { target: 'http://localhost:8787', changeOrigin: true },
    },
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./tests/setup.ts'],
    include: ['tests/**/*.test.{ts,tsx}'],
  },
})
