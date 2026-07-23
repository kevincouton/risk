export default defineNuxtConfig({
  app: {
    head: {
      titleTemplate: '%s — risk',
      htmlAttrs: { lang: 'en' },
    },
  },
  devtools: { enabled: false },
  modules: ['@nuxtjs/tailwindcss'],
  runtimeConfig: {
    public: {
      apiUrl: process.env.NUXT_PUBLIC_API_URL || 'http://localhost:8080',
      siteUrl: process.env.NUXT_PUBLIC_SITE_URL || 'https://risk.lucanian.app',
      siteName: 'risk',
      adsEnabled: process.env.NUXT_PUBLIC_ADS_ENABLED || 'false',
      adsProvider: process.env.NUXT_PUBLIC_ADS_PROVIDER || 'none',
      adsenseClientId: process.env.NUXT_PUBLIC_ADSENSE_CLIENT_ID || '',
    },
  },
  nitro: {
    prerender: {
      routes: ['/', '/about'],
    },
  },
})
