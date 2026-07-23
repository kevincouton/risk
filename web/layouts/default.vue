<template>
  <div
    class="min-h-screen bg-gray-50 text-gray-900 transition-colors dark:bg-gray-950 dark:text-gray-100"
  >
    <a
      href="#main-content"
      class="sr-only focus:not-sr-only focus:absolute focus:left-4 focus:top-4 focus:z-50 focus:rounded-lg focus:bg-blue-600 focus:px-4 focus:py-2 focus:text-white focus:shadow-lg"
    >
      Skip to main content
    </a>

    <header class="relative border-b bg-white dark:border-gray-800 dark:bg-gray-900">
      <div class="max-w-6xl mx-auto px-4 py-4 flex items-center justify-between">
        <NuxtLink
          to="/"
          class="text-xl font-bold text-gray-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 rounded-lg dark:text-gray-100"
        >
          risk
        </NuxtLink>

        <div class="flex items-center gap-2">
          <nav class="hidden md:flex items-center gap-6 text-sm" aria-label="Main navigation">
            <NuxtLink
              to="/"
              class="text-gray-600 hover:text-gray-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 rounded-md dark:text-gray-400 dark:hover:text-gray-100"
              active-class="font-medium text-gray-900 dark:text-gray-100"
            >
              Entities
            </NuxtLink>
            <NuxtLink
              to="/about"
              class="text-gray-600 hover:text-gray-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 rounded-md dark:text-gray-400 dark:hover:text-gray-100"
              active-class="font-medium text-gray-900 dark:text-gray-100"
            >
              About
            </NuxtLink>
          </nav>

          <ThemeToggle />

          <MobileNav>
            <NuxtLink
              to="/"
              class="px-3 py-2 rounded-lg text-sm text-gray-600 hover:bg-gray-50 hover:text-gray-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-100"
              active-class="font-medium text-gray-900 bg-gray-50 dark:text-gray-100 dark:bg-gray-800"
              @click="$event.target.closest('#mobile-menu')?.querySelector('button')?.click?.()"
            >
              Entities
            </NuxtLink>
            <NuxtLink
              to="/about"
              class="px-3 py-2 rounded-lg text-sm text-gray-600 hover:bg-gray-50 hover:text-gray-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-100"
              active-class="font-medium text-gray-900 bg-gray-50 dark:text-gray-100 dark:bg-gray-800"
              @click="$event.target.closest('#mobile-menu')?.querySelector('button')?.click?.()"
            >
              About
            </NuxtLink>
          </MobileNav>
        </div>
      </div>
    </header>

    <main id="main-content" class="max-w-6xl mx-auto px-4 py-6 md:py-8" tabindex="-1">
      <slot />
    </main>

    <div class="max-w-6xl mx-auto px-4 mb-8">
      <AdPlaceholder slot-id="risk-footer-leaderboard" format="leaderboard" label="Sponsored" />
    </div>

    <footer class="border-t bg-white mt-12 dark:border-gray-800 dark:bg-gray-900">
      <div class="max-w-6xl mx-auto px-4 py-6 text-sm text-gray-500 dark:text-gray-400">
        risk — Built with Nuxt 3 &amp; Go.
      </div>
    </footer>
  </div>
</template>
