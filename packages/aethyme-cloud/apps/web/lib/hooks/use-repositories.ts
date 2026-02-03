import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import apiClient from '@/lib/api-client'

interface Repository {
  id: string
  name: string
  full_name: string
  url: string
  provider: 'github' | 'gitlab' | 'bitbucket'
  is_private: boolean
  is_indexed: boolean
  indexing_status: 'pending' | 'indexing' | 'completed' | 'failed'
  created_at: string
}

interface RepositoryCreate {
  name: string
  full_name: string
  url: string
  provider: 'github' | 'gitlab' | 'bitbucket'
  provider_id: string
  is_private?: boolean
}

interface RepositoryListResponse {
  items: Repository[]
  total: number
  page: number
  page_size: number
  has_next: boolean
  has_prev: boolean
}

export function useRepositories(page = 1, pageSize = 10) {
  return useQuery({
    queryKey: ['repositories', page, pageSize],
    queryFn: async () => {
      const { data } = await apiClient.get<RepositoryListResponse>('/repositories/', {
        params: { page, page_size: pageSize },
      })
      return data
    },
  })
}

export function useCreateRepository() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (repoData: RepositoryCreate) => {
      const { data } = await apiClient.post<Repository>('/repositories/', repoData)
      return data
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['repositories'] })
    },
  })
}

export function useDeleteRepository() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (id: string) => {
      await apiClient.delete(`/repositories/${id}`)
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['repositories'] })
    },
  })
}

export function useReindexRepository() {
  return useMutation({
    mutationFn: async (id: string) => {
      const { data } = await apiClient.post(`/repositories/${id}/reindex`)
      return data
    },
  })
}
