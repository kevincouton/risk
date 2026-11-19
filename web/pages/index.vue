<template>
  <div>
    <div class="mb-10">
      <h1 class="text-4xl font-extrabold mb-3 tracking-tight">Entities</h1>
      <p class="text-gray-500 text-lg">Scored and ranked with the risk methodology.</p>
    </div>

    <div v-if="pending" class="text-gray-500 py-12 text-center">Loading...</div>
    <div v-else-if="error" class="text-red-600 py-12 text-center">Error: {{ error }}</div>
    <div v-else class="grid gap-5 md:grid-cols-2 lg:grid-cols-3">
      <EntityCard v-for="entity in entities" :key="entity.id" :entity="entity" />
    </div>
  </div>
</template>

<script setup>
useSeo({
  title: 'Risk — Scored open-source dependency directory',
  description:
    'Risk scores open-source dependencies on release trajectory, maintenance signals, and documentation quality — a composite 0-100 score with a green/yellow/red/critical verdict for every package.',
  keywords: [
    'dependency health',
    'open source risk',
    'maintenance signals',
    'dependency scoring',
    'supply chain',
    'Risk',
  ],
})

useHead({
  script: [
    {
      type: 'application/ld+json',
      innerHTML: JSON.stringify({
        '@context': 'https://schema.org',
        '@type': 'WebSite',
        name: 'Risk',
        url: 'https://risk.lucanian.app',
        description:
          'Scores open-source dependencies on release trajectory, maintenance signals, and documentation quality.',
      }),
    },
  ],
})

const { getEntities } = useApi()

const { data, pending, error } = await useAsyncData('entities', () => getEntities({ limit: '50' }))

const entities = computed(() => data.value?.entities || [])
</script>
