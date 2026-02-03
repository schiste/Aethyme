import { useCallback,useEffect, useState } from 'react'

export interface SavedSearch {
  id: string
  name: string
  query: string
  filters: {
    languages?: string[]
    repositories?: string[]
    file_path?: string
  }
  created_at: Date
  last_used?: Date
  use_count: number
  is_favorite: boolean
}

const STORAGE_KEY = 'aethyme_saved_searches'
const MAX_SAVED_SEARCHES = 50

export function useSavedSearches() {
  const [savedSearches, setSavedSearches] = useState<SavedSearch[]>([])

  // Load from localStorage on mount
  useEffect(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY)
      if (stored) {
        const parsed = JSON.parse(stored)
        const searchesWithDates = parsed.map((search: any) => ({
          ...search,
          created_at: new Date(search.created_at),
          last_used: search.last_used ? new Date(search.last_used) : undefined,
        }))
        setSavedSearches(searchesWithDates)
      }
    } catch (error) {
      console.error('Failed to load saved searches:', error)
    }
  }, [])

  // Save to localStorage whenever searches change
  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(savedSearches))
    } catch (error) {
      console.error('Failed to save searches:', error)
    }
  }, [savedSearches])

  const saveSearch = useCallback((
    name: string,
    query: string,
    filters: SavedSearch['filters'],
    isFavorite: boolean = false
  ) => {
    if (!name.trim() || !query.trim()) return

    setSavedSearches((prev) => {
      // Check if search with same name exists
      const existingIndex = prev.findIndex((s) => s.name === name)

      if (existingIndex >= 0) {
        // Update existing search
        const updated = [...prev]
        updated[existingIndex] = {
          ...updated[existingIndex],
          query,
          filters,
          last_used: new Date(),
          is_favorite: isFavorite,
        }
        return updated
      }

      // Add new search
      const newSearch: SavedSearch = {
        id: crypto.randomUUID(),
        name,
        query,
        filters,
        created_at: new Date(),
        use_count: 0,
        is_favorite: isFavorite,
      }

      return [newSearch, ...prev].slice(0, MAX_SAVED_SEARCHES)
    })
  }, [])

  const deleteSearch = useCallback((id: string) => {
    setSavedSearches((prev) => prev.filter((search) => search.id !== id))
  }, [])

  const updateSearch = useCallback((
    id: string,
    updates: Partial<Pick<SavedSearch, 'name' | 'query' | 'filters' | 'is_favorite'>>
  ) => {
    setSavedSearches((prev) =>
      prev.map((search) =>
        search.id === id ? { ...search, ...updates } : search
      )
    )
  }, [])

  const recordSearchUse = useCallback((id: string) => {
    setSavedSearches((prev) =>
      prev.map((search) =>
        search.id === id
          ? {
              ...search,
              last_used: new Date(),
              use_count: search.use_count + 1,
            }
          : search
      )
    )
  }, [])

  const toggleFavorite = useCallback((id: string) => {
    setSavedSearches((prev) =>
      prev.map((search) =>
        search.id === id
          ? { ...search, is_favorite: !search.is_favorite }
          : search
      )
    )
  }, [])

  const clearAllSearches = useCallback(() => {
    if (confirm('Are you sure you want to delete all saved searches?')) {
      setSavedSearches([])
      localStorage.removeItem(STORAGE_KEY)
    }
  }, [])

  const getFavorites = useCallback(() => {
    return savedSearches.filter((search) => search.is_favorite)
  }, [savedSearches])

  const getMostUsed = useCallback((limit: number = 5) => {
    return [...savedSearches]
      .sort((a, b) => b.use_count - a.use_count)
      .slice(0, limit)
  }, [savedSearches])

  const getRecentlyUsed = useCallback((limit: number = 5) => {
    return [...savedSearches]
      .filter((search) => search.last_used)
      .sort((a, b) => {
        if (!a.last_used) return 1
        if (!b.last_used) return -1
        return b.last_used.getTime() - a.last_used.getTime()
      })
      .slice(0, limit)
  }, [savedSearches])

  return {
    savedSearches,
    saveSearch,
    deleteSearch,
    updateSearch,
    recordSearchUse,
    toggleFavorite,
    clearAllSearches,
    getFavorites,
    getMostUsed,
    getRecentlyUsed,
  }
}
