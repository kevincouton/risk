import tailwindcss from '@tailwindcss/vite'

export default defineNuxtConfig({
  app: {
    head: {
      titleTemplate: '%s — risk',
      htmlAttrs: { lang: 'en' },
    },
  },
  devtools: { enabled: false },
  runtimeConfig: {
    public: {
      apiUrl: process.env.NUXT_PUBLIC_API_URL || 'http://localhost:8080',
      siteUrl: process.env.NUXT_PUBLIC_SITE_URL || 'https://risk.lucanian.app',
      siteName: 'risk',
      adsEnabled: process.env.NUXT_PUBLIC_ADS_ENABLED || 'false',
      adsProvider: process.env.NUXT_PUBLIC_ADS_PROVIDER || 'none',
      adsenseClientId: process.env.NUXT_PUBLIC_ADSENSE_CLIENT_ID || '',
      posthogKey: process.env.NUXT_PUBLIC_POSTHOG_KEY || '',
      posthogHost: process.env.NUXT_PUBLIC_POSTHOG_HOST || 'https://eu.posthog.com',
    },
  },
  nitro: {
    prerender: {
      routes: ['/', '/about'],
    },
  },
  css: ['~/assets/css/main.css'],
  vite: {
    plugins: [tailwindcss()],
  },
})
