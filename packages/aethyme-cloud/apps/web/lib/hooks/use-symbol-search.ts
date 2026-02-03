import { useCallback, useEffect,useState } from 'react'

import {
  type SymbolResult,
  symbolSearchApi,
  type SymbolSearchRequest,
} from '../api/symbol-search'

interface UseSymbolSearchOptions {
  autoSearch?: boolean
  per_page?: number
  debounceMs?: number
}

interface UseSymbolSearchReturn {
  // State
  query: string
  results: SymbolResult[]
  total: number
  page: number
  totalPages: number
  limit: number
  isLoading: boolean
  error: string | null

  // Filters
  repositoryId?: string
  kind?: string
  language?: string
  filePath?: string

  // Actions
  setQuery: (query: string) => void
  search: (query: string) => Promise<void>
  nextPage: () => void
  prevPage: () => void
  goToPage: (page: number) => void

  // Filter actions
  setRepositoryId: (id?: string) => void
  setKind: (kind?: string) => void
  setLanguage: (language?: string) => void
  setFilePath: (path?: string) => void
  clearFilters: () => void
}

export function useSymbolSearch(
  options: UseSymbolSearchOptions = {}
): UseSymbolSearchReturn {
  const { autoSearch = false, per_page = 20, debounceMs = 300 } = options

  // State
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<SymbolResult[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(1)
  const [limit, _setLimit] = useState(per_page)
  const [isLoading, setIsLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Filters
  const [repositoryId, setRepositoryId] = useState<string | undefined>()
  const [kind, setKind] = useState<string | undefined>()
  const [language, setLanguage] = useState<string | undefined>()
  const [filePath, setFilePath] = useState<string | undefined>()

  // Debounce timer
  const [debounceTimer, setDebounceTimer] = useState<NodeJS.Timeout | null>(null)

  // Calculate total pages
  const totalPages = Math.ceil(total / limit)

  // Search function
  const search = useCallback(
    async (searchQuery: string) => {
      if (!searchQuery.trim()) {
        setResults([])
        setTotal(0)
        setError(null)
        return
      }

      setIsLoading(true)
      setError(null)

      try {
        const params: SymbolSearchRequest = {
          query: searchQuery,
          repository_id: repositoryId,
          kind,
          language,
          file_path: filePath,
          page,
          per_page: limit,
        }

        const response = await symbolSearchApi.searchSymbols(params)

        setResults(response.results)
        setTotal(response.total)
      } catch (err) {
        setError(err instanceof Error ? err.message : 'An error occurred during search')
        setResults([])
        setTotal(0)
      } finally {
        setIsLoading(false)
      }
    },
    [repositoryId, kind, language, filePath, page, limit]
  )

  // Auto-search when query or filters change (with debounce)
  useEffect(() => {
    if (autoSearch && query) {
      if (debounceTimer) {
        clearTimeout(debounceTimer)
      }

      const timer = setTimeout(() => {
        search(query)
      }, debounceMs)

      setDebounceTimer(timer)

      return () => {
        clearTimeout(timer)
      }
    }
  }, [query, repositoryId, kind, language, filePath, page, autoSearch, debounceMs, debounceTimer, search])

  // Pagination
  const nextPage = useCallback(() => {
    if (page < totalPages) {
      setPage(page + 1)
    }
  }, [page, totalPages])

  const prevPage = useCallback(() => {
    if (page > 1) {
      setPage(page - 1)
    }
  }, [page])

  const goToPage = useCallback((newPage: number) => {
    if (newPage >= 1 && newPage <= totalPages) {
      setPage(newPage)
    }
  }, [totalPages])

  // Filter actions
  const clearFilters = useCallback(() => {
    setRepositoryId(undefined)
    setKind(undefined)
    setLanguage(undefined)
    setFilePath(undefined)
    setPage(1)
  }, [])

  // Reset to page 1 when filters change
  useEffect(() => {
    setPage(1)
  }, [repositoryId, kind, language, filePath])

  return {
    // State
    query,
    results,
    total,
    page,
    totalPages,
    limit,
    isLoading,
    error,

    // Filters
    repositoryId,
    kind,
    language,
    filePath,

    // Actions
    setQuery,
    search,
    nextPage,
    prevPage,
    goToPage,

    // Filter actions
    setRepositoryId,
    setKind,
    setLanguage,
    setFilePath,
    clearFilters,
  }
}
