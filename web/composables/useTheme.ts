export type Theme = 'light' | 'dark' | 'system'

const THEME_KEY = 'theme'

function getSystemTheme(): 'light' | 'dark' {
  if (typeof window !== 'undefined' && window.matchMedia) {
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
  }
  return 'light'
}

function getStoredTheme(): Theme | null {
  if (typeof localStorage !== 'undefined') {
    const stored = localStorage.getItem(THEME_KEY)
    if (stored === 'light' || stored === 'dark' || stored === 'system') {
      return stored
    }
  }
  return null
}

function applyTheme(theme: Theme) {
  const resolved = theme === 'system' ? getSystemTheme() : theme
  const html = document.documentElement
  if (resolved === 'dark') {
    html.classList.add('dark')
  } else {
    html.classList.remove('dark')
  }
}

export function useTheme() {
  const theme = ref<Theme>(getStoredTheme() || 'system')
  const isDark = computed(() => {
    const resolved = theme.value === 'system' ? getSystemTheme() : theme.value
    return resolved === 'dark'
  })

  const setTheme = (value: Theme) => {
    theme.value = value
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem(THEME_KEY, value)
    }
    applyTheme(value)
  }

  const toggleTheme = () => {
    setTheme(isDark.value ? 'light' : 'dark')
  }

  onMounted(() => {
    applyTheme(theme.value)

    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const handler = () => {
      if (theme.value === 'system') {
        applyTheme('system')
      }
    }
    media.addEventListener('change', handler)
    onBeforeUnmount(() => {
      media.removeEventListener('change', handler)
    })
  })

  return {
    theme: readonly(theme),
    isDark,
    setTheme,
    toggleTheme,
  }
}
