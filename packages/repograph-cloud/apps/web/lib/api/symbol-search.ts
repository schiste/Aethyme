import apiClient from '../api-client'

export interface SymbolSearchRequest {
  query: string
  repository_id?: string
  kind?: string // function, class, method, variable, import
  language?: string
  file_path?: string
  page?: number
  per_page?: number
}

export interface SymbolResult {
  symbol_id: string
  kind: string
  name: string
  file_path: string
  line_start: number
  line_end: number
  signature?: string
  documentation?: string
  repository_name: string
  language: string
  score: number
}

export interface SymbolSearchResponse {
  query: string
  results: SymbolResult[]
  total: number
  limit: number
  offset: number
  filters: {
    repository_id?: string
    kind?: string
    language?: string
    file_path?: string
  }
}

export interface SymbolDetail {
  symbol_id: string
  kind: string
  name: string
  file_path: string
  line_start: number
  line_end: number
  col_start: number
  col_end: number
  language: string
  signature?: string
  documentation?: string
  content?: string
  parent_symbol_id?: string
  references: Array<{
    type: string
    target_symbol_id: string
  }>
  repository_id: string
  repository_name: string
  indexed_at: string
}

export interface RepositoryStatistics {
  repository_id: string
  statistics: {
    total_symbols: number
    by_kind: Record<string, number>
    by_language: Record<string, number>
    file_count: number
    files: Record<string, number>
  }
}

export const symbolSearchApi = {
  /**
   * Search for symbols across all repositories
   */
  async searchSymbols(params: SymbolSearchRequest): Promise<SymbolSearchResponse> {
    const queryParams = new URLSearchParams()
    queryParams.append('q', params.query)

    if (params.repository_id) {
      queryParams.append('repository_id', params.repository_id)
    }

    if (params.kind) {
      queryParams.append('kind', params.kind)
    }

    if (params.language) {
      queryParams.append('language', params.language)
    }

    if (params.file_path) {
      queryParams.append('file_path', params.file_path)
    }

    const limit = params.per_page || 20
    const page = params.page || 1
    const offset = (page - 1) * limit

    queryParams.append('limit', limit.toString())
    queryParams.append('offset', offset.toString())

    const { data } = await apiClient.get<SymbolSearchResponse>(
      `/search?${queryParams.toString()}`
    )

    return data
  },

  /**
   * Get detailed information about a specific symbol
   */
  async getSymbol(symbolId: string): Promise<SymbolDetail> {
    const { data } = await apiClient.get<SymbolDetail>(`/symbols/${encodeURIComponent(symbolId)}`)
    return data
  },

  /**
   * Get code statistics for a repository
   */
  async getRepositoryStatistics(repositoryId: string): Promise<RepositoryStatistics> {
    const { data } = await apiClient.get<RepositoryStatistics>(
      `/repositories/${repositoryId}/statistics`
    )
    return data
  },

  /**
   * Check Elasticsearch health
   */
  async checkHealth(): Promise<{ elasticsearch: string; status: string }> {
    const { data } = await apiClient.get<{ elasticsearch: string; status: string }>(
      '/search/health'
    )
    return data
  },
}
