<template>
  <div class="min-h-screen bg-gray-50">
    <header class="bg-white border-b">
      <div class="max-w-6xl mx-auto px-4 py-4 flex items-center justify-between">
        <NuxtLink to="/" class="text-xl font-bold text-gray-900"> risk </NuxtLink>
        <nav class="flex gap-6 text-sm">
          <NuxtLink
            to="/"
            class="text-gray-600 hover:text-gray-900"
            active-class="font-medium text-gray-900"
            >Entities</NuxtLink
          >
          <NuxtLink
            to="/about"
            class="text-gray-600 hover:text-gray-900"
            active-class="font-medium text-gray-900"
            >About</NuxtLink
          >
        </nav>
      </div>
    </header>
    <main class="max-w-6xl mx-auto px-4 py-8">
      <slot />
    </main>
    <footer class="border-t bg-white mt-12">
      <div class="max-w-6xl mx-auto px-4 py-6 text-sm text-gray-500">
        risk — Built with Nuxt 3 & Go.
      </div>
    </footer>
  </div>
</template>
