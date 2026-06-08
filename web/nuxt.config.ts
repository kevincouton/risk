export default defineNuxtConfig({
  devtools: { enabled: false },
  modules: ["@nuxtjs/tailwindcss"],
  runtimeConfig: {
    public: {
      apiUrl: process.env.NUXT_PUBLIC_API_URL || "http://localhost:8080",
    },
  },
  nitro: {
    prerender: {
      routes: ["/", "/about"],
    },
  },
});
