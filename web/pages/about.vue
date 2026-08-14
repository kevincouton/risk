<template>
  <div class="max-w-3xl">
    <h1 class="text-3xl md:text-4xl font-extrabold mb-6 tracking-tight">About risk</h1>
    <p class="text-lg leading-relaxed text-gray-600 dark:text-gray-300">
      risk is a crowdsourced intelligence platform. It empowers users to build, share, and discover
      across the platform.
    </p>

    <JsonLd :data="jsonLd" />
  </div>
</template>

<script setup>
useSeo({
  title: 'About — risk',
  description:
    'Learn about risk, a crowdsourced intelligence platform for scoring, ranking, and discovering entities.',
  keywords: ['about', 'risk', 'methodology', 'scoring', 'ranking'],
})

const jsonLd = {
  '@context': 'https://schema.org',
  '@type': 'AboutPage',
  name: 'About risk',
  description: 'Learn about risk, a crowdsourced intelligence platform.',
  url: 'https://risk.lucanian.app/about',
  mainEntity: {
    '@type': 'Organization',
    name: 'risk',
    url: 'https://risk.lucanian.app',
  },
}
</script>
