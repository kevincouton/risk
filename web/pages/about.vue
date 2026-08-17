<template>
  <div class="max-w-3xl">
    <h1 class="text-3xl md:text-4xl font-extrabold mb-6 tracking-tight">About risk</h1>
    <p class="text-lg leading-relaxed text-gray-600 dark:text-gray-300">
      risk scores the supply-chain risk of open-source dependencies across the npm, PyPI, and
      crates.io ecosystems.
    </p>

    <h2 class="text-xl font-bold mt-8 mb-3">What we track</h2>
    <p class="leading-relaxed text-gray-600 dark:text-gray-300">
      Each entry is a dependency-ecosystem package discovered through its GitHub repository. For
      every package we record community adoption (GitHub stars), maintenance activity (last push),
      and issue pressure (open issues relative to stars). Via the deps.dev API we add the number
      of published security advisories affecting the package's default release and how stale that
      release is.
    </p>

    <h2 class="text-xl font-bold mt-8 mb-3">How the score works</h2>
    <p class="leading-relaxed text-gray-600 dark:text-gray-300">
      The composite score (0–100) blends popularity, maintenance recency, and issue pressure.
      Advisory counts and version freshness from deps.dev are published alongside the score in
      each package's metadata, so known-vulnerability exposure is visible before you adopt a
      dependency. Data is refreshed every six hours.
    </p>

    <JsonLd :data="jsonLd" />
  </div>
</template>

<script setup>
useSeo({
  title: 'About — risk',
  description:
    'How risk scores open-source dependency risk: GitHub adoption and maintenance signals plus deps.dev advisory counts and version freshness across npm, PyPI, and crates.io.',
  keywords: [
    'dependency risk',
    'supply chain security',
    'npm',
    'pypi',
    'crates.io',
    'deps.dev',
    'security advisories',
  ],
})

const jsonLd = {
  '@context': 'https://schema.org',
  '@type': 'AboutPage',
  name: 'About risk',
  description: 'Dependency-risk scoring across npm, PyPI, and crates.io packages.',
  url: 'https://risk.lucanian.app/about',
  mainEntity: {
    '@type': 'Organization',
    name: 'risk',
    url: 'https://risk.lucanian.app',
  },
}
</script>
