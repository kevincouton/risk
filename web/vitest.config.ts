import { defineVitestConfig } from '@nuxt/test-utils/config'

export default defineVitestConfig({
  // Playwright e2e specs live in tests/e2e and must not run under vitest.
  test: { environment: 'jsdom', include: ['tests/unit/**/*.spec.ts'] },
})
