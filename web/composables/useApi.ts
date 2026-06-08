export const useApi = () => {
  const config = useRuntimeConfig()
  const baseURL = config.public.apiUrl

  const getEntities = async (params: Record<string, string> = {}) => {
    const query = new URLSearchParams(params).toString()
    return $fetch(`${baseURL}/api/v1/entities${query ? '?' + query : ''}`)
  }

  const getEntity = async (owner: string, name: string, platform?: string) => {
    const qs = platform ? `?platform=${encodeURIComponent(platform)}` : ''
    return $fetch(`${baseURL}/api/v1/entities/${owner}/${name}${qs}`)
  }

  const search = async (q: string) => {
    return $fetch(`${baseURL}/api/v1/search?q=${encodeURIComponent(q)}`)
  }

  return { getEntities, getEntity, search }
}
