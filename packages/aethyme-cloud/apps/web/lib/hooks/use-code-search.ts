import { useCallback, useEffect,useState } from 'react'
import { useDebounce } from 'use-debounce'

import { searchApi, type SearchRequest, type SearchResponse, type SearchResultItem } from '../api/search'

interface UseCodeSearchOptions {
  initialQuery?: string
  repositories?: string[]
  languages?: string[]
  file_path?: string
  per_page?: number
  autoSearch?: boolean
  debounceMs?: number
}

interface UseCodeSearchReturn {
  query: string
  setQuery: (query: string) => void
  results: SearchResultItem[]
  total: number
  page: number
  totalPages: number
  perPage: number
  facets: SearchResponse['facets'] | null
  isLoading: boolean
  error: string | null
  tookMs: number
  search: (newQuery?: string) => Promise<void>
  nextPage: () => void
  prevPage: () => void
  goToPage: (page: number) => void
  setRepositories: (repos: string[]) => void
  setLanguages: (langs: string[]) => void
  setFilePath: (path: string) => void
  clearFilters: () => void
}

export function useCodeSearch(options: UseCodeSearchOptions = {}): UseCodeSearchReturn {
  const {
    initialQuery = '',
    repositories: initialRepositories = [],
    languages: initialLanguages = [],
    file_path: initialFilePath = '',
    per_page = 20,
    autoSearch = false,
    debounceMs = 500
  } = options

  // Search state
  const [query, setQuery] = useState(initialQuery)
  const [debouncedQuery] = useDebounce(query, debounceMs)
  const [repositories, setRepositories] = useState<string[]>(initialRepositories)
  const [languages, setLanguages] = useState<string[]>(initialLanguages)
  const [filePath, setFilePath] = useState(initialFilePath)
  const [page, setPage] = useState(1)

  // Results state
  const [results, setResults] = useState<SearchResultItem[]>([])
  const [total, setTotal] = useState(0)
  const [totalPages, setTotalPages] = useState(0)
  const [facets, setFacets] = useState<SearchResponse['facets'] | null>(null)
  const [tookMs, setTookMs] = useState(0)

  // UI state
  const [isLoading, setIsLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const search = useCallback(async (newQuery?: string) => {
    const searchQuery = newQuery !== undefined ? newQuery : query

    if (!searchQuery.trim()) {
      setResults([])
      setTotal(0)
      setTotalPages(0)
      setFacets(null)
      setTookMs(0)
      return
    }

    setIsLoading(true)
    setError(null)

    try {
      const params: SearchRequest = {
        query: searchQuery,
        page,
        per_page,
        repositories: repositories.length > 0 ? repositories : undefined,
        languages: languages.length > 0 ? languages : undefined,
        file_path: filePath || undefined
      }

      const response = await searchApi.search(params)

      setResults(response.results)
      setTotal(response.total)
      setTotalPages(response.total_pages)
      setFacets(response.facets)
      setTookMs(response.took_ms)
    } catch (err: any) {
      const errorMsg = err.response?.data?.detail || err.message || 'Search failed'
      setError(errorMsg)
      setResults([])
      setTotal(0)
      setTotalPages(0)
      setFacets(null)
    } finally {
      setIsLoading(false)
    }
  }, [query, page, per_page, repositories, languages, filePath])

  // Auto-search on debounced query change
  useEffect(() => {
    if (autoSearch && debouncedQuery) {
      search()
    }
  }, [debouncedQuery, autoSearch, search])

  // Auto-search on filter changes
  useEffect(() => {
    if (autoSearch && query) {
      setPage(1)
      search()
    }
  }, [repositories, languages, filePath, autoSearch, query, search])

  // Auto-search on page change
  useEffect(() => {
    if (autoSearch && query && page > 1) {
      search()
    }
  }, [page, autoSearch, query, search])

  const nextPage = useCallback(() => {
    if (page < totalPages) {
      setPage(p => p + 1)
    }
  }, [page, totalPages])

  const prevPage = useCallback(() => {
    if (page > 1) {
      setPage(p => p - 1)
    }
  }, [page])

  const goToPage = useCallback((newPage: number) => {
    if (newPage >= 1 && newPage <= totalPages) {
      setPage(newPage)
    }
  }, [totalPages])

  const clearFilters = useCallback(() => {
    setRepositories([])
    setLanguages([])
    setFilePath('')
    setPage(1)
  }, [])

  return {
    query,
    setQuery,
    results,
    total,
    page,
    totalPages,
    perPage: per_page,
    facets,
    isLoading,
    error,
    tookMs,
    search,
    nextPage,
    prevPage,
    goToPage,
    setRepositories,
    setLanguages,
    setFilePath,
    clearFilters
  }
}
