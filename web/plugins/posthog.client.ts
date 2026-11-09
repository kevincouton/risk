import posthog from 'posthog-js'
import type { PostHog } from 'posthog-js'

export default defineNuxtPlugin(() => {
  const config = useRuntimeConfig()
  const key = config.public.posthogKey
  const host = config.public.posthogHost || 'https://eu.posthog.com'

  let client: PostHog | null = null

  if (key) {
    client = posthog.init(key, { api_host: host }) ?? null
  }

  const getClient = (): PostHog | null => client

  return {
    provide: { posthog: getClient },
  }
})
