<template>
  <div class="md:hidden">
    <button
      type="button"
      :aria-expanded="isOpen"
      aria-controls="mobile-menu"
      aria-label="Toggle navigation menu"
      class="inline-flex items-center justify-center rounded-lg p-2 text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-100"
      @click="isOpen = !isOpen"
    >
      <svg
        v-if="!isOpen"
        xmlns="http://www.w3.org/2000/svg"
        class="h-6 w-6"
        fill="none"
        viewBox="0 0 24 24"
        stroke="currentColor"
        stroke-width="2"
        aria-hidden="true"
      >
        <path stroke-linecap="round" stroke-linejoin="round" d="M4 6h16M4 12h16M4 18h16" />
      </svg>
      <svg
        v-else
        xmlns="http://www.w3.org/2000/svg"
        class="h-6 w-6"
        fill="none"
        viewBox="0 0 24 24"
        stroke="currentColor"
        stroke-width="2"
        aria-hidden="true"
      >
        <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
      </svg>
    </button>

    <transition
      enter-active-class="transition duration-200 ease-out"
      enter-from-class="-translate-y-2 opacity-0"
      enter-to-class="translate-y-0 opacity-100"
      leave-active-class="transition duration-150 ease-in"
      leave-from-class="translate-y-0 opacity-100"
      leave-to-class="-translate-y-2 opacity-0"
    >
      <div
        v-show="isOpen"
        id="mobile-menu"
        ref="menuRef"
        class="absolute left-0 right-0 top-full z-50 border-b bg-white shadow-lg dark:border-gray-800 dark:bg-gray-900"
      >
        <nav class="flex flex-col gap-1 px-4 py-3" aria-label="Mobile navigation">
          <slot />
        </nav>
      </div>
    </transition>
  </div>
</template>

<script setup>
const isOpen = ref(false)
const menuRef = ref(null)

function onKeydown(e) {
  if (e.key === 'Escape') {
    isOpen.value = false
  }
}

function onClickOutside(e) {
  if (
    menuRef.value &&
    !menuRef.value.contains(e.target) &&
    !e.target.closest('button[aria-controls="mobile-menu"]')
  ) {
    isOpen.value = false
  }
}

onMounted(() => {
  document.addEventListener('keydown', onKeydown)
  document.addEventListener('click', onClickOutside)
})

onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKeydown)
  document.removeEventListener('click', onClickOutside)
})
</script>
