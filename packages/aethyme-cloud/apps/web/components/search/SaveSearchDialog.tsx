'use client'

import { Save, Star,X } from 'lucide-react'
import { useState } from 'react'

interface SaveSearchDialogProps {
  query: string
  filters: {
    languages?: string[]
    repositories?: string[]
    file_path?: string
  }
  onSave: (name: string, markAsFavorite: boolean) => void
  onClose: () => void
}

export function SaveSearchDialog({ query, filters, onSave, onClose }: SaveSearchDialogProps) {
  const [name, setName] = useState('')
  const [markAsFavorite, setMarkAsFavorite] = useState(false)

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (name.trim()) {
      onSave(name.trim(), markAsFavorite)
      onClose()
    }
  }

  const hasFilters =
    (filters.languages?.length || 0) > 0 ||
    (filters.repositories?.length || 0) > 0 ||
    !!filters.file_path

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
      <div className="bg-gray-800 border border-gray-700 rounded-lg max-w-md w-full shadow-xl">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-gray-700">
          <h2 className="text-lg font-semibold text-white">Save Search</h2>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-gray-700 text-gray-400 hover:text-gray-200"
          >
            <X className="h-5 w-5" />
          </button>
        </div>

        {/* Body */}
        <form onSubmit={handleSubmit} className="p-4 space-y-4">
          {/* Query preview */}
          <div>
            <div className="block text-sm font-medium text-gray-300 mb-1">
              Search Query
            </div>
            <div className="px-3 py-2 bg-gray-900 border border-gray-700 rounded text-sm text-gray-400">
              &quot;{query}&quot;
            </div>
          </div>

          {/* Filters preview */}
          {hasFilters && (
            <div>
              <div className="block text-sm font-medium text-gray-300 mb-1">
                Applied Filters
              </div>
              <div className="px-3 py-2 bg-gray-900 border border-gray-700 rounded space-y-1">
                {filters.languages && filters.languages.length > 0 && (
                  <div className="flex flex-wrap gap-1">
                    {filters.languages.map((lang) => (
                      <span
                        key={lang}
                        className="inline-flex items-center px-2 py-0.5 rounded text-xs bg-blue-900/30 text-blue-300"
                      >
                        {lang}
                      </span>
                    ))}
                  </div>
                )}
                {filters.repositories && filters.repositories.length > 0 && (
                  <div className="flex flex-wrap gap-1">
                    {filters.repositories.map((repo) => (
                      <span
                        key={repo}
                        className="inline-flex items-center px-2 py-0.5 rounded text-xs bg-purple-900/30 text-purple-300"
                      >
                        {repo}
                      </span>
                    ))}
                  </div>
                )}
                {filters.file_path && (
                  <div className="text-xs text-gray-400">
                    Path: {filters.file_path}
                  </div>
                )}
              </div>
            </div>
          )}

          {/* Name input */}
          <div>
            <label htmlFor="search-name" className="block text-sm font-medium text-gray-300 mb-1">
              Search Name <span className="text-red-400">*</span>
            </label>
      <input
              id="search-name"
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g., 'Authentication functions' or 'Database models'"
              className="w-full px-3 py-2 bg-gray-900 border border-gray-700 rounded text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
              autoFocus
              required
            />
          </div>

          {/* Mark as favorite */}
          <div>
            <label htmlFor="form-control-0" className="flex items-center space-x-2 cursor-pointer">
              <input
                type="checkbox"
                checked={markAsFavorite}
                onChange={(e) => setMarkAsFavorite(e.target.checked)}
                className="rounded border-gray-600 text-yellow-500 focus:ring-yellow-500 focus:ring-offset-gray-800"
              />
              <span className="text-sm text-gray-300 flex items-center space-x-1">
                <Star className={`h-4 w-4 ${markAsFavorite ? 'text-yellow-400 fill-current' : 'text-gray-400'}`} />
                <span>Mark as favorite</span>
              </span>
            </label>
          </div>

          {/* Actions */}
          <div className="flex items-center justify-end space-x-3 pt-2">
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 text-sm text-gray-300 hover:text-white transition-colors"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={!name.trim()}
              className="px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 focus:ring-offset-gray-800 disabled:opacity-50 disabled:cursor-not-allowed text-sm font-medium flex items-center space-x-2"
            >
              <Save className="h-4 w-4" />
              <span>Save Search</span>
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}
