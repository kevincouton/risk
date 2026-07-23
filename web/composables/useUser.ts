import { computed } from 'vue'
// Imported explicitly (not Nuxt auto-import) so the composable always uses
// plain ofetch with absolute API URLs — and so unit tests can mock 'ofetch'
// (the Nuxt $fetch wrapper requires a running Nuxt app).
import { $fetch } from 'ofetch'

export interface PlatformUser {
  id: string
  email: string
  display_name: string
  groups: string[]
  premium: boolean
}

export const useUser = () => {
  const config = useRuntimeConfig()
  const user = useState<PlatformUser | null>('platform-user', () => null)

  const isPremium = computed(
    () => !!user.value && (user.value.premium || (user.value.groups ?? []).includes('premium'))
  )

  const fetchUser = async () => {
    try {
      user.value = await $fetch<PlatformUser>(`${config.public.apiUrl}/auth/me`, {
        credentials: 'include',
      })
    } catch {
      user.value = null
    }
  }

  const login = () => {
    window.location.href = `${config.public.apiUrl}/auth/login`
  }

  const logout = async () => {
    try {
      await $fetch(`${config.public.apiUrl}/auth/logout`, {
        method: 'POST',
        credentials: 'include',
      })
    } finally {
      user.value = null
    }
  }

  return { user, isPremium, fetchUser, login, logout }
}
