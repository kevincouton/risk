<template>
  <div
    class="bg-white rounded-2xl border p-5 hover:shadow-lg transition-shadow group dark:border-gray-800 dark:bg-gray-900"
    @click="onClick"
  >
    <NuxtLink :to="`/${entity.full_name}`" class="block">
      <div class="flex items-start justify-between mb-3">
        <h3
          class="font-semibold text-lg leading-tight group-hover:text-blue-700 transition-colors dark:text-gray-100"
        >
          {{ entity.full_name }}
        </h3>
        <span
          class="shrink-0 ml-3 px-2 py-0.5 rounded-full text-[10px] font-bold uppercase tracking-wide"
          :class="verdictBadgeClass(entity.verdict)"
        >
          {{ entity.verdict }}
        </span>
      </div>
      <p class="text-gray-500 text-sm line-clamp-2 leading-relaxed dark:text-gray-400">
        {{ entity.description }}
      </p>
      <div class="flex items-center gap-3 mt-4 text-xs text-gray-400">
        <span
          v-if="entity.category"
          class="bg-gray-100 px-2.5 py-1 rounded-lg font-medium text-gray-600 dark:bg-gray-800 dark:text-gray-300"
        >
          {{ entity.category }}
        </span>
        <span class="font-mono font-semibold text-gray-700 dark:text-gray-300"
          >{{ entity.composite_score }}/100</span
        >
      </div>
    </NuxtLink>
  </div>
</template>

<script setup>
const props = defineProps({
  entity: { type: Object, required: true },
})

const { trackEntityClick } = useAnalytics()

function onClick() {
  trackEntityClick(props.entity)
}

function verdictBadgeClass(v) {
  return {
    'bg-green-100 text-green-700': v === 'green',
    'bg-yellow-100 text-yellow-700': v === 'yellow',
    'bg-red-100 text-red-700': v === 'red',
    'bg-gray-100 text-gray-700': v === 'critical',
  }
}
</script>
