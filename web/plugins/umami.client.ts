// Umami dual-run tracker — additive alongside PostHog until cutover.
// Dashboard: https://analytics.lucanian.app (Authentik-protected);
// /script.js + /api/send are public so site snippets work.
export default defineNuxtPlugin(() => {
  const config = useRuntimeConfig()
  const id = config.public.umamiWebsiteId as string | undefined

  if (!id || document.querySelector('script[data-website-id]')) return

  const script = document.createElement('script')
  script.defer = true
  script.src = 'https://analytics.lucanian.app/script.js'
  script.dataset.websiteId = id
  document.head.appendChild(script)
})
