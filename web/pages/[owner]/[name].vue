<template>
  <div>
    <div v-if="pending" class="text-gray-500 py-12 text-center">Loading...</div>
    <div v-else-if="error" class="text-red-600 py-12 text-center">Error loading entity</div>
    <div v-else-if="entity">
      <div class="mb-6">
        <NuxtLink to="/" class="text-sm text-gray-500 hover:text-gray-900">← Back</NuxtLink>
      </div>
      <div class="bg-white rounded-2xl border p-8 mb-8">
        <div class="flex items-start justify-between mb-6">
          <div class="flex-1">
            <h1 class="text-3xl font-bold tracking-tight">{{ entity.full_name }}</h1>
            <p class="text-gray-500 text-lg leading-relaxed mt-2">{{ entity.description }}</p>
          </div>
        </div>
        <div class="grid grid-cols-2 md:grid-cols-4 gap-4">
          <div class="bg-gray-50 px-5 py-4 rounded-xl text-center">
            <div class="text-gray-400 text-xs font-medium uppercase tracking-wider mb-1">Score</div>
            <div class="text-2xl font-extrabold">{{ entity.composite_score }}/100</div>
          </div>
          <div class="bg-gray-50 px-5 py-4 rounded-xl text-center">
            <div class="text-gray-400 text-xs font-medium uppercase tracking-wider mb-1">Value</div>
            <div class="text-2xl font-extrabold">{{ entity.score_value }}</div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
const { getEntity } = useApi()
const route = useRoute()
const owner = route.params.owner
const name = route.params.name

const { data: entity, pending, error } = await useAsyncData(
  `entity-${owner}-${name}`,
  () => getEntity(owner, name)
)
</script>
