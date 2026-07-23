import { defineConfig } from 'vite-plus'

export default defineConfig({
  lint: {
    ignorePatterns: [
      '.nuxt/**',
      '.output/**',
      'dist/**',
      'node_modules/**',
      'playwright.config.ts',
      'test-results/**',
      'e2e/**',
    ],
  },
  fmt: {
    semi: false,
    singleQuote: true,
    trailingComma: 'es5',
  },
})
