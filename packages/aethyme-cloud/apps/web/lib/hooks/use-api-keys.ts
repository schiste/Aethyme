import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import apiClient from '@/lib/api-client'

interface APIKey {
  id: string
  name: string
  key_prefix: string
  scopes: string[] | null
  is_active: boolean
  created_at: string
  expires_at: string | null
  last_used_at: string | null
}

interface APIKeyCreate {
  name: string
  scopes?: string[]
  expires_in_days?: number
}

interface APIKeyCreateResponse extends APIKey {
  key: string // Only returned on creation
}

interface APIKeyListResponse {
  items: APIKey[]
  total: number
}

export function useAPIKeys() {
  return useQuery({
    queryKey: ['api-keys'],
    queryFn: async () => {
      const { data } = await apiClient.get<APIKeyListResponse>('/api-keys/')
      return data
    },
  })
}

export function useCreateAPIKey() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (keyData: APIKeyCreate) => {
      const { data } = await apiClient.post<APIKeyCreateResponse>('/api-keys/', keyData)
      return data
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['api-keys'] })
    },
  })
}

export function useRevokeAPIKey() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (id: string) => {
      await apiClient.delete(`/api-keys/${id}`)
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['api-keys'] })
    },
  })
}
