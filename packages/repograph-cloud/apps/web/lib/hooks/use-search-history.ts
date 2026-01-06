import { useCallback,useEffect, useState } from 'react'

export interface SearchHistoryItem {
  id: string
  query: string
  filters: {
    languages?: string[]
    repositories?: string[]
    file_path?: string
  }
  timestamp: Date
  results_count?: number
}

const STORAGE_KEY = 'repograph_search_history'
const MAX_HISTORY_ITEMS = 50

/**
 * Hook for managing search history
 * Stores search queries with filters and results count in localStorage
 */
export function useSearchHistory() {
  const [history, setHistory] = useState<SearchHistoryItem[]>([])

  // Load history from localStorage on mount
  useEffect(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY)
      if (stored) {
        const parsed = JSON.parse(stored)
        // Convert timestamp strings back to Date objects
        const historyWithDates = parsed.map((item: any) => ({
          ...item,
          timestamp: new Date(item.timestamp)
        }))
        setHistory(historyWithDates)
      }
    } catch (error) {
      console.error('Failed to load search history:', error)
    }
  }, [])

  // Save history to localStorage whenever it changes
  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(history))
    } catch (error) {
      console.error('Failed to save search history:', error)
    }
  }, [history])

  const addSearch = useCallback((
    query: string,
    filters: SearchHistoryItem['filters'],
    resultsCount?: number
  ) => {
    if (!query.trim()) return

    setHistory((prev) => {
      // Check if this exact search already exists (same query and filters)
      const isDuplicate = prev.some((item) => {
        const sameQuery = item.query === query
        const sameFilters = JSON.stringify(item.filters) === JSON.stringify(filters)
        return sameQuery && sameFilters
      })

      // If duplicate, move it to the top and update timestamp/count
      if (isDuplicate) {
        return [
          {
            id: crypto.randomUUID(),
            query,
            filters,
            timestamp: new Date(),
            results_count: resultsCount
          },
          ...prev.filter((item) => {
            const sameQuery = item.query !== query
            const sameFilters = JSON.stringify(item.filters) !== JSON.stringify(filters)
            return sameQuery || sameFilters
          })
        ].slice(0, MAX_HISTORY_ITEMS)
      }

      // Add new search to the beginning
      return [
        {
          id: crypto.randomUUID(),
          query,
          filters,
          timestamp: new Date(),
          results_count: resultsCount
        },
        ...prev
      ].slice(0, MAX_HISTORY_ITEMS)
    })
  }, [])

  const deleteSearch = useCallback((id: string) => {
    setHistory((prev) => prev.filter((item) => item.id !== id))
  }, [])

  const clearHistory = useCallback(() => {
    setHistory([])
    localStorage.removeItem(STORAGE_KEY)
  }, [])

  const getRecentSearches = useCallback((limit: number = 10) => {
    return history.slice(0, limit)
  }, [history])

  return {
    history,
    recentSearches: getRecentSearches(),
    addSearch,
    deleteSearch,
    clearHistory
  }
}
