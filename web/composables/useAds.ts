export interface AdConfig {
  enabled: boolean
  provider: string | null
  clientId: string | null
}

interface AdsPublicConfig {
  adsEnabled?: string
  adsProvider?: string
  adsenseClientId?: string
}

// Pure resolver, exported for unit tests.
export const resolveAdConfig = (publicCfg: AdsPublicConfig): AdConfig => {
  const providerRaw = publicCfg.adsProvider || 'none'
  return {
    enabled: publicCfg.adsEnabled === 'true',
    provider: providerRaw === 'none' ? null : providerRaw,
    clientId: publicCfg.adsenseClientId || null,
  }
}

export const useAds = (): AdConfig => {
  const config = useRuntimeConfig()
  return resolveAdConfig(config.public as AdsPublicConfig)
}
