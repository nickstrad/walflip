import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// base './' so the built site works from any sub-path or straight from dist/.
export default defineConfig({
  base: './',
  plugins: [react()],
  // The ELK layout worker alone is about 2 MB; that is expected.
  build: { chunkSizeWarningLimit: 2500 },
})
