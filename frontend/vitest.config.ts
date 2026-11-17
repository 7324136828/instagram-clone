import { defineConfig } from 'vitest/config';

// Unit tests for the import library. Kept separate from vite.config.ts so that the app build
// configuration stays untouched. Node 20+ provides File, Blob and URL.createObjectURL.
export default defineConfig({
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
