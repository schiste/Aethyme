'use client'

import { formatDistanceToNow } from 'date-fns'
import {Search, Star, Trash2 } from 'lucide-react'
import { useState } from 'react'

import { type SavedSearch,useSavedSearches } from '@/lib/hooks/use-saved-searches'

interface SavedSearchesPanelProps {
  onSelectSearch: (search: SavedSearch) => void
}

export function SavedSearchesPanel({ onSelectSearch }: SavedSearchesPanelProps) {
  const {
    savedSearches,
    deleteSearch,
    toggleFavorite,
    clearAllSearches,
    getFavorites,
    getMostUsed,
  } = useSavedSearches()

  const [activeTab, setActiveTab] = useState<'all' | 'favorites' | 'most-used'>('all')
  const [_editingId, _setEditingId] = useState<string | null>(null)

  const getDisplaySearches = () => {
    switch (activeTab) {
      case 'favorites':
        return getFavorites()
      case 'most-used':
        return getMostUsed(10)
      default:
        return savedSearches
    }
  }

  const displaySearches = getDisplaySearches()

  if (savedSearches.length === 0) {
    return (
      <div className="border border-gray-700 rounded-lg p-6 bg-gray-800/50 text-center">
        <Search className="h-12 w-12 text-gray-600 mx-auto mb-3" />
        <p className="text-sm text-gray-400 mb-1">No saved searches yet</p>
        <p className="text-xs text-gray-500">
          Save your frequent searches for quick access
        </p>
      </div>
    )
  }

  return (
    <div className="border border-gray-700 rounded-lg bg-gray-800/50 overflow-hidden">
      {/* Header with tabs */}
      <div className="border-b border-gray-700 bg-gray-800 p-3">
        <div className="flex items-center justify-between mb-2">
          <h3 className="text-sm font-medium text-gray-200">Saved Searches</h3>
          <button
            onClick={clearAllSearches}
            className="text-xs text-gray-500 hover:text-red-400"
          >
            Clear all
          </button>
        </div>

        {/* Tabs */}
        <div className="flex space-x-1 text-xs">
          <button
            onClick={() => setActiveTab('all')}
            className={`px-3 py-1 rounded ${
              activeTab === 'all'
                ? 'bg-blue-600 text-white'
                : 'text-gray-400 hover:text-gray-200'
            }`}
          >
            All ({savedSearches.length})
          </button>
          <button
            onClick={() => setActiveTab('favorites')}
            className={`px-3 py-1 rounded ${
              activeTab === 'favorites'
                ? 'bg-blue-600 text-white'
                : 'text-gray-400 hover:text-gray-200'
            }`}
          >
            ⭐ Favorites ({getFavorites().length})
          </button>
          <button
            onClick={() => setActiveTab('most-used')}
            className={`px-3 py-1 rounded ${
              activeTab === 'most-used'
                ? 'bg-blue-600 text-white'
                : 'text-gray-400 hover:text-gray-200'
            }`}
          >
            Most Used
          </button>
        </div>
      </div>

      {/* Search list */}
      <div className="divide-y divide-gray-700 max-h-96 overflow-y-auto">
        {displaySearches.map((search) => (
          <div
            key={search.id}
            className="p-3 hover:bg-gray-700/50 transition-colors group"
          >
            <div className="flex items-start space-x-3">
              {/* Favorite button */}
              <button
                onClick={(e) => {
                  e.stopPropagation()
                  toggleFavorite(search.id)
                }}
                className={`mt-0.5 ${
                  search.is_favorite ? 'text-yellow-400' : 'text-gray-600 hover:text-yellow-400'
                }`}
              >
                <Star className={`h-4 w-4 ${search.is_favorite ? 'fill-current' : ''}`} />
              </button>

              {/* Search content */}
              <div
                className="flex-1 min-w-0 cursor-pointer"
                onClick={() => onSelectSearch(search)}
              >
                <div className="flex items-center space-x-2 mb-1">
                  <h4 className="text-sm font-medium text-gray-200 truncate">
                    {search.name}
                  </h4>
                  {search.use_count > 0 && (
                    <span className="text-xs text-gray-500 flex-shrink-0">
                      {search.use_count} uses
                    </span>
                  )}
                </div>

                <p className="text-xs text-gray-400 mb-1 truncate">
                  "{search.query}"
                </p>

                {/* Filters */}
                {(search.filters.languages?.length ||
                  search.filters.repositories?.length ||
                  search.filters.file_path) && (
                  <div className="flex flex-wrap gap-1 mb-1">
                    {search.filters.languages?.slice(0, 2).map((lang) => (
                      <span
                        key={lang}
                        className="inline-flex items-center px-1.5 py-0.5 rounded text-xs bg-blue-900/30 text-blue-300"
                      >
                        {lang}
                      </span>
                    ))}
                    {(search.filters.languages?.length || 0) > 2 && (
                      <span className="text-xs text-gray-500">
                        +{(search.filters.languages?.length || 0) - 2}
                      </span>
                    )}
                    {search.filters.repositories?.slice(0, 1).map((repo) => (
                      <span
                        key={repo}
                        className="inline-flex items-center px-1.5 py-0.5 rounded text-xs bg-purple-900/30 text-purple-300"
                      >
                        {repo.split('/').pop()}
                      </span>
                    ))}
                    {(search.filters.repositories?.length || 0) > 1 && (
                      <span className="text-xs text-gray-500">
                        +{(search.filters.repositories?.length || 0) - 1}
                      </span>
                    )}
                  </div>
                )}

                {/* Metadata */}
                <div className="flex items-center space-x-2 text-xs text-gray-500">
                  <span>
                    Created {formatDistanceToNow(search.created_at, { addSuffix: true })}
                  </span>
                  {search.last_used && (
                    <>
                      <span>•</span>
                      <span>
                        Used {formatDistanceToNow(search.last_used, { addSuffix: true })}
                      </span>
                    </>
                  )}
                </div>
              </div>

              {/* Actions */}
              <div className="flex items-center space-x-1 opacity-0 group-hover:opacity-100 transition-opacity">
                <button
                  onClick={(e) => {
                    e.stopPropagation()
                    deleteSearch(search.id)
                  }}
                  className="p-1 rounded hover:bg-gray-600 text-gray-400 hover:text-red-400"
                  title="Delete search"
                >
                  <Trash2 className="h-3 w-3" />
                </button>
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
