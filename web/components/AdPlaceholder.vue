<template>
  <ins
    v-if="showAdsense"
    class="adsbygoogle"
    style="display: block"
    :data-ad-client="ads.clientId"
    :data-ad-slot="slotId"
    data-ad-format="auto"
    data-full-width-responsive="true"
  />
  <div
    v-else-if="showPlaceholder"
    :class="[
      'ad-slot rounded-lg border border-dashed flex items-center justify-center text-center',
      sizeClasses,
      className,
    ]"
    :data-ad-slot="slotId"
    :data-ad-format="format"
  >
    <div class="text-gray-400 dark:text-gray-500 text-xs">
      <span class="uppercase tracking-wider font-semibold">{{ label }}</span>
      <span v-if="isDev" class="block mt-1 opacity-60">{{ slotId }} | {{ format }}</span>
    </div>
  </div>
</template>

<script setup>
import { computed, onMounted } from 'vue'

import { useAds } from '../composables/useAds'

const props = defineProps({
  slotId: { type: String, required: true },
  format: {
    type: String,
    default: 'responsive',
    validator: (v) =>
      ['responsive', 'rectangle', 'leaderboard', 'skyscraper', 'native'].includes(v),
  },
  label: { type: String, default: 'Advertisement' },
  className: { type: String, default: '' },
})

const ads = useAds()
const isDev = import.meta.dev

const showAdsense = computed(() => ads.enabled && ads.provider === 'adsense' && !!ads.clientId)
const showPlaceholder = computed(() => ads.enabled && !showAdsense.value)

if (showAdsense.value) {
  useHead({
    script: [
      {
        src: `https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js?client=${ads.clientId}`,
        async: true,
        crossorigin: 'anonymous',
      },
    ],
  })
}

onMounted(() => {
  if (import.meta.client && showAdsense.value) {
    ;(window.adsbygoogle = window.adsbygoogle || []).push({})
  }
})

const sizeClasses = computed(() => {
  switch (props.format) {
    case 'rectangle':
      return 'w-[300px] h-[250px]'
    case 'leaderboard':
      return 'w-full max-w-[728px] h-[90px]'
    case 'skyscraper':
      return 'w-[160px] h-[600px] hidden lg:flex'
    case 'native':
      return 'w-full h-auto min-h-[120px] py-4'
    default:
      return 'w-full min-h-[90px] py-3'
  }
})
</script>
