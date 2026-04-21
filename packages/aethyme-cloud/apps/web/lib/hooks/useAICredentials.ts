import { useEffect,useState } from 'react'

import apiClient from '@/lib/api-client'

interface AICredential {
  id: number
  provider_name: string
  provider_type: string
  is_active: boolean
  is_validated: boolean
  last_validated_at: string | null
  total_requests: number
  total_tokens_used: number
  created_at: string
}

interface AddCredentialParams {
  provider_type: string
  provider_name: string
  credentials: {
    api_key: string
    azure_endpoint?: string
    azure_deployment?: string
  }
  validate?: boolean
}

export function useAICredentials() {
  const [credentials, setCredentials] = useState<AICredential[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const fetchCredentials = async () => {
    try {
      setLoading(true)
      setError(null)
      const response = await apiClient.get('/api/v1/ai/credentials')
      setCredentials(response.data)
    } catch (err: any) {
      setError(err.message || 'Failed to load credentials')
    } finally {
      setLoading(false)
    }
  }

  const addCredential = async (params: AddCredentialParams) => {
    const response = await apiClient.post('/api/v1/ai/credentials', params)
    await fetchCredentials()
    return response.data
  }

  const deleteCredential = async (id: number) => {
    await apiClient.delete(`/api/v1/ai/credentials/${id}`)
    await fetchCredentials()
  }

  const revalidateCredential = async (id: number) => {
    await apiClient.post(`/api/v1/ai/credentials/${id}/validate`)
    await fetchCredentials()
  }

  useEffect(() => {
    fetchCredentials()
  }, [])

  return {
    credentials,
    loading,
    error,
    addCredential,
    deleteCredential,
    revalidateCredential,
    refresh: fetchCredentials,
  }
}
