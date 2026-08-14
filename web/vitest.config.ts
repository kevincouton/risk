import { defineVitestConfig } from '@nuxt/test-utils/config'

export default defineVitestConfig({
  // Playwright e2e specs live in tests/e2e and must not run under vitest.
  // risk has no unit tests yet; pass cleanly until tests/unit exists.
  test: {
    environment: 'jsdom',
    include: ['tests/unit/**/*.spec.ts'],
    passWithNoTests: true,
  },
})
