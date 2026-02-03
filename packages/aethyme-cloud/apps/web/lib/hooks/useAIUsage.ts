import { useEffect,useState } from 'react'

import { apiClient } from '@/lib/api-client'

interface AIUsage {
  total_tokens: number
  total_requests: number
  estimated_cost: number
  change_percentage: number
  daily_usage: Array<{
    date: string
    tokens: number
    requests: number
  }>
  by_provider: Record<
    string,
    {
      tokens: number
      requests: number
      cost: number
    }
  >
}

export function useAIUsage() {
  const [usage, setUsage] = useState<AIUsage | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const fetchUsage = async () => {
    try {
      setLoading(true)
      setError(null)
      const response = await apiClient.get('/api/v1/ai/usage')
      setUsage(response.data)
    } catch (err: any) {
      setError(err.message || 'Failed to load usage data')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    fetchUsage()
  }, [])

  return {
    usage,
    loading,
    error,
    refresh: fetchUsage,
  }
}
