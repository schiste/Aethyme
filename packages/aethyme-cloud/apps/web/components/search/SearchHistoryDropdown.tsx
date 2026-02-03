'use client'

import { formatDistanceToNow } from 'date-fns'
import { Clock, Trash2,X } from 'lucide-react'
import { useEffect,useRef } from 'react'

import { type SearchHistoryItem,useSearchHistory } from '@/lib/hooks/use-search-history'

interface SearchHistoryDropdownProps {
  onSelectSearch: (item: SearchHistoryItem) => void
  show: boolean
  onClose: () => void
}

export function SearchHistoryDropdown({ onSelectSearch, show, onClose }: SearchHistoryDropdownProps) {
  const { recentSearches, deleteSearch, clearHistory } = useSearchHistory()
  const dropdownRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        onClose()
      }
    }

    if (show) {
      document.addEventListener('mousedown', handleClickOutside)
    }

    return () => {
      document.removeEventListener('mousedown', handleClickOutside)
    }
  }, [show, onClose])

  if (!show) return null

  if (recentSearches.length === 0) {
    return (
      <div
        ref={dropdownRef}
        className="absolute top-full left-0 right-0 mt-2 bg-gray-800 border border-gray-700 rounded-lg shadow-lg z-50 p-4"
      >
        <p className="text-sm text-gray-400 text-center">No search history yet</p>
      </div>
    )
  }

  return (
    <div
      ref={dropdownRef}
      className="absolute top-full left-0 right-0 mt-2 bg-gray-800 border border-gray-700 rounded-lg shadow-lg z-50 max-h-96 overflow-y-auto"
    >
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-gray-700 sticky top-0 bg-gray-800">
        <div className="flex items-center space-x-2">
          <Clock className="h-4 w-4 text-gray-400" />
          <h3 className="text-sm font-medium text-gray-200">Recent Searches</h3>
        </div>
        <button
          onClick={(e) => {
            e.stopPropagation()
            if (confirm('Clear all search history?')) {
              clearHistory()
            }
          }}
          className="text-xs text-gray-400 hover:text-red-400 flex items-center space-x-1"
        >
          <Trash2 className="h-3 w-3" />
          <span>Clear all</span>
        </button>
      </div>

      {/* History items */}
      <div className="divide-y divide-gray-700">
        {recentSearches.map((item) => (
          <div
            key={item.id}
            className="p-3 hover:bg-gray-700/50 cursor-pointer group transition-colors"
          >
            <div className="flex items-start justify-between">
              <div
                className="flex-1 min-w-0"
                onClick={() => {
                  onSelectSearch(item)
                  onClose()
                }}
              >
                {/* Query */}
                <div className="flex items-center space-x-2 mb-1">
                  <p className="text-sm font-medium text-gray-200 truncate">
                    "{item.query}"
                  </p>
                  {item.results_count !== undefined && (
                    <span className="text-xs text-gray-500">
                      ({item.results_count} results)
                    </span>
                  )}
                </div>

                {/* Filters */}
                {(item.filters.languages?.length || item.filters.repositories?.length || item.filters.file_path) && (
                  <div className="flex flex-wrap gap-1 mb-1">
                    {item.filters.languages?.map((lang) => (
                      <span
                        key={lang}
                        className="inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-blue-900/30 text-blue-300"
                      >
                        {lang}
                      </span>
                    ))}
                    {item.filters.repositories?.map((repo) => (
                      <span
                        key={repo}
                        className="inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-purple-900/30 text-purple-300"
                      >
                        {repo}
                      </span>
                    ))}
                    {item.filters.file_path && (
                      <span className="inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-green-900/30 text-green-300">
                        path: {item.filters.file_path}
                      </span>
                    )}
                  </div>
                )}

                {/* Timestamp */}
                <p className="text-xs text-gray-500">
                  {formatDistanceToNow(item.timestamp, { addSuffix: true })}
                </p>
              </div>

              {/* Delete button */}
              <button
                onClick={(e) => {
                  e.stopPropagation()
                  deleteSearch(item.id)
                }}
                className="ml-2 p-1 rounded hover:bg-gray-600 text-gray-400 hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
