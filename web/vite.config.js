import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// `base: './'` keeps asset URLs relative so the SPA works whatever path the
// Rust server mounts it on. Output goes to `web/dist`, embedded by rust-embed.
export default defineConfig({
  plugins: [vue()],
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
})
