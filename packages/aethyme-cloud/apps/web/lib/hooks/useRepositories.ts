import { useEffect,useState } from 'react'

import apiClient from '@/lib/api-client'

interface Repository {
  id: string
  name: string
  full_name: string
  provider: string
  url: string
  default_branch: string
  is_private: boolean
  status: string
  indexed_at: string | null
  created_at: string
}

export function useRepositories() {
  const [repositories, setRepositories] = useState<Repository[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const fetchRepositories = async () => {
    try {
      setLoading(true)
      setError(null)
      const response = await apiClient.get('/api/v1/repositories')
      setRepositories(response.data)
    } catch (err: any) {
      setError(err.message || 'Failed to load repositories')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    fetchRepositories()
  }, [])

  return {
    repositories,
    loading,
    error,
    refresh: fetchRepositories,
  }
}
