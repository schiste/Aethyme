import apiClient from '../api-client'

export interface SearchRequest {
  query: string
  repositories?: string[]
  languages?: string[]
  file_path?: string
  page?: number
  per_page?: number
  sort_by?: string
}

export interface SearchResultItem {
  repository_id: string
  repository_name: string
  file_path: string
  file_name: string
  language: string
  score: number
  highlights: string[]
  line_count: number
  size_bytes: number
  indexed_at: string
}

export interface SearchFacets {
  languages: Record<string, number>
  repositories: Record<string, number>
}

export interface SearchResponse {
  query: string
  total: number
  page: number
  per_page: number
  total_pages: number
  results: SearchResultItem[]
  facets: SearchFacets
  took_ms: number
}

export interface FileContentResponse {
  repository_id: string
  file_path: string
  language: string
  content: string
  line_count: number
  size_bytes: number
}

export const searchApi = {
  /**
   * Search code across all repositories
   */
  async search(params: SearchRequest): Promise<SearchResponse> {
    const queryParams = new URLSearchParams()
    queryParams.append('q', params.query)

    if (params.repositories && params.repositories.length > 0) {
      params.repositories.forEach(repo => queryParams.append('repositories', repo))
    }

    if (params.languages && params.languages.length > 0) {
      params.languages.forEach(lang => queryParams.append('languages', lang))
    }

    if (params.file_path) {
      queryParams.append('file_path', params.file_path)
    }

    queryParams.append('page', (params.page || 1).toString())
    queryParams.append('per_page', (params.per_page || 20).toString())

    if (params.sort_by) {
      queryParams.append('sort_by', params.sort_by)
    }

    const { data } = await apiClient.get<SearchResponse>(
      `/search/?${queryParams.toString()}`
    )
    return data
  },

  /**
   * Search within a specific repository
   */
  async searchRepository(
    repositoryId: string,
    params: Omit<SearchRequest, 'repositories'>
  ): Promise<SearchResponse> {
    const queryParams = new URLSearchParams()
    queryParams.append('q', params.query)

    if (params.languages && params.languages.length > 0) {
      params.languages.forEach(lang => queryParams.append('languages', lang))
    }

    if (params.file_path) {
      queryParams.append('file_path', params.file_path)
    }

    queryParams.append('page', (params.page || 1).toString())
    queryParams.append('per_page', (params.per_page || 20).toString())

    const { data } = await apiClient.get<SearchResponse>(
      `/search/repositories/${repositoryId}?${queryParams.toString()}`
    )
    return data
  },

  /**
   * Get file content for preview
   */
  async getFileContent(
    repositoryId: string,
    filePath: string
  ): Promise<FileContentResponse> {
    const { data } = await apiClient.get<FileContentResponse>(
      `/search/repositories/${repositoryId}/files`,
      {
        params: { file_path: filePath }
      }
    )
    return data
  }
}
