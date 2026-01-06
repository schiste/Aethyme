import apiClient from '../api-client'

export interface CreateRepositoryRequest {
  name: string
  url: string
  github_id: string
  description?: string | null
  language?: string | null
  stars?: number
  forks?: number
  is_private?: boolean
  default_branch?: string
}

export interface Repository {
  id: string
  organization_id: string
  name: string
  full_name: string
  url: string
  description?: string | null
  provider: string
  provider_id: string
  default_branch: string
  is_private: boolean
  is_indexed: boolean
  indexing_status: string
  github_id?: string
  clone_url?: string
  stars: number
  forks: number
  total_files: number
  total_lines: number
  languages?: Record<string, number>
  primary_language?: string
  indexed_at?: string
  created_at: string
  updated_at: string
}

export interface FileTreeNode {
  name: string
  path: string
  type: 'file' | 'folder'
  language?: string | null
  size_bytes?: number | null
  line_count?: number | null
  children?: FileTreeNode[] | null
}

export interface RepositoryTreeResponse {
  repository_id: string
  repository_name: string
  total_files: number
  total_folders: number
  tree: FileTreeNode[]
  indexed_at?: string | null
}

export interface RepositoryStatsResponse {
  repository_id: string
  name: string
  full_name: string
  url: string
  description?: string | null
  stars: number
  forks: number
  total_files: number
  total_lines: number
  languages: Record<string, number>
  primary_language?: string | null
  indexed_at?: string | null
  created_at: string
  updated_at: string
}

export const repositoriesApi = {
  /**
   * Create a new repository
   */
  async create(data: CreateRepositoryRequest): Promise<Repository> {
    const { data: repository } = await apiClient.post<Repository>(
      '/repositories/',
      {
        name: data.name,
        full_name: data.name, // Will be updated to owner/repo format
        url: data.url,
        description: data.description,
        provider: 'github',
        provider_id: data.github_id,
        github_id: data.github_id,
        clone_url: data.url,
        default_branch: data.default_branch || 'main',
        is_private: data.is_private || false,
        stars: data.stars || 0,
        forks: data.forks || 0,
        primary_language: data.language,
      }
    )
    return repository
  },

  /**
   * List all repositories
   */
  async list(): Promise<Repository[]> {
    const { data } = await apiClient.get<Repository[]>('/repositories/')
    return data
  },

  /**
   * Get repository by ID
   */
  async get(id: string): Promise<Repository> {
    const { data } = await apiClient.get<Repository>(`/repositories/${id}`)
    return data
  },

  /**
   * Delete repository
   */
  async delete(id: string): Promise<void> {
    await apiClient.delete(`/repositories/${id}`)
  },

  /**
   * Get repository file tree
   */
  async getTree(repositoryId: string): Promise<RepositoryTreeResponse> {
    const { data } = await apiClient.get<RepositoryTreeResponse>(`/repositories/${repositoryId}/tree`)
    return data
  },

  /**
   * Get repository statistics
   */
  async getStats(repositoryId: string): Promise<RepositoryStatsResponse> {
    const { data } = await apiClient.get<RepositoryStatsResponse>(`/repositories/${repositoryId}/stats`)
    return data
  }
}
