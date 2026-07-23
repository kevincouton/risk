export const useAnalytics = () => {
  const { $posthog, $gtag } = useNuxtApp()

  const track = (event: string, properties?: Record<string, any>) => {
    try {
      $posthog?.().capture(event, properties)
    } catch {
      /* silent fail */
    }
    try {
      $gtag?.('event', event, properties)
    } catch {
      /* silent fail */
    }
  }

  const trackEntityClick = (entity: {
    full_name: string
    composite_score: number
    verdict: string
  }) => {
    track('entity_click', {
      entity: entity.full_name,
      score: entity.composite_score,
      verdict: entity.verdict,
    })
  }

  const trackSearch = (query: string, resultCount: number) => {
    track('search', { query, result_count: resultCount })
  }

  return { track, trackEntityClick, trackSearch }
}
