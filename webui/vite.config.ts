import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

// `base: './'` keeps every emitted asset reference relative so mediaboxd can
// serve webui/dist from any mount point without rewriting the bundle.
export default defineConfig({
  base: './',
  plugins: [react()],
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: false,
  },
  server: {
    port: 5173,
    // Only used by `npm run dev`; production serves the API from the same origin.
    proxy: {
      '/api': { target: 'http://localhost:8080', changeOrigin: true, ws: true },
    },
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./tests/setup.ts'],
    include: ['tests/**/*.test.{ts,tsx}'],
  },
})
