<template>
  <component :is="'script'" type="application/ld+json">
    {{ json }}
  </component>
</template>

<script setup>
const props = defineProps({
  data: {
    type: Object,
    required: true,
  },
})

const json = computed(() => JSON.stringify(props.data))
</script>
