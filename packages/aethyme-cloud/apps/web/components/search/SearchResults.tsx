'use client'

import React, { useState } from 'react'

import { CodePreview } from '@/components/code/CodePreview'
import { type SearchResultItem } from '@/lib/api/search'
import { sanitizeSearchHighlight } from '@/lib/sanitizeHtml'

interface SearchResultsProps {
  results: SearchResultItem[]
  query: string
  isLoading: boolean
  error: string | null
  total: number
  page: number
  totalPages: number
  tookMs: number
  onNextPage: () => void
  onPrevPage: () => void
  onGoToPage: (page: number) => void
}

export function SearchResults({
  results,
  query,
  isLoading,
  error,
  total,
  page,
  totalPages,
  tookMs,
  onNextPage,
  onPrevPage,
  onGoToPage
}: SearchResultsProps) {
  const [expandedResults, setExpandedResults] = useState<Set<string>>(new Set())

  const toggleExpanded = (resultId: string) => {
    setExpandedResults(prev => {
      const next = new Set(prev)
      if (next.has(resultId)) {
        next.delete(resultId)
      } else {
        next.add(resultId)
      }
      return next
    })
  }

  if (error) {
    return (
      <div className="border border-red-800 rounded-lg p-6 bg-red-900/20">
        <h3 className="text-lg font-semibold text-red-400 mb-2">Search Error</h3>
        <p className="text-sm text-red-300">{error}</p>
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="space-y-4">
        {[...Array(5)].map((_, i) => (
          <div key={i} className="border border-gray-700 rounded-lg p-6 bg-gray-800/50 animate-pulse">
            <div className="h-4 bg-gray-700 rounded w-3/4 mb-3"></div>
            <div className="h-3 bg-gray-700 rounded w-1/2 mb-2"></div>
            <div className="h-3 bg-gray-700 rounded w-2/3"></div>
          </div>
        ))}
      </div>
    )
  }

  if (!query) {
    return (
      <div className="text-center py-12">
        <svg
          className="mx-auto h-12 w-12 text-gray-500"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          aria-hidden="true"
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z"
          />
        </svg>
        <h3 className="mt-4 text-lg font-medium text-gray-300">Search Your Code</h3>
        <p className="mt-2 text-sm text-gray-500">
          Enter a search query to find code across your repositories
        </p>
      </div>
    )
  }

  if (results.length === 0) {
    return (
      <div className="text-center py-12">
        <svg
          className="mx-auto h-12 w-12 text-gray-500"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          aria-hidden="true"
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M9.172 16.172a4 4 0 015.656 0M9 10h.01M15 10h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"
          />
        </svg>
        <h3 className="mt-4 text-lg font-medium text-gray-300">No results found</h3>
        <p className="mt-2 text-sm text-gray-500">
          Try adjusting your search query or filters
        </p>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Results header */}
      <div className="flex items-center justify-between text-sm text-gray-400">
        <span>
          {total.toLocaleString()} {total === 1 ? 'result' : 'results'} for &ldquo;{query}&rdquo;
          {tookMs > 0 && <span className="ml-2">({tookMs.toFixed(0)}ms)</span>}
        </span>
      </div>

      {/* Results list */}
      <div className="space-y-4">
        {results.map((result, index) => {
          const resultId = `${result.repository_id}-${result.file_path}-${index}`
          const isExpanded = expandedResults.has(resultId)

          return (
            <div
              key={resultId}
              className="border border-gray-700 rounded-lg bg-gray-800/50 hover:border-gray-600 transition-colors"
            >
              {/* Result header */}
              <div className="p-4">
                <div className="flex items-start justify-between mb-2">
                  <div className="flex-1">
                    <h3 className="text-base font-mono text-blue-400 hover:text-blue-300 cursor-pointer">
                      {result.file_name}
                    </h3>
                    <p className="text-sm text-gray-500 font-mono mt-1">
                      {result.repository_name} / {result.file_path}
                    </p>
                  </div>
                  <div className="flex items-center space-x-2 ml-4">
                    <span className="text-xs text-gray-500 uppercase px-2 py-1 bg-gray-700 rounded">
                      {result.language}
                    </span>
                    <span className="text-xs text-gray-500">
                      Score: {result.score.toFixed(2)}
                    </span>
                  </div>
                </div>

                {/* Highlights */}
                {result.highlights.length > 0 && (
                  <div className="mt-3 space-y-2">
                    {result.highlights.map((highlight, i) => (
                      <div
                        key={i}
                        className="bg-gray-900 rounded px-3 py-2 text-sm font-mono text-gray-300 border border-gray-700"
                      >
                        <span dangerouslySetInnerHTML={{ __html: sanitizeSearchHighlight(highlight) }} />
                      </div>
                    ))}
                  </div>
                )}

                {/* Metadata */}
                <div className="mt-3 flex items-center justify-between">
                  <div className="flex items-center space-x-4 text-xs text-gray-500">
                    <span>{result.line_count} lines</span>
                    <span>{(result.size_bytes / 1024).toFixed(1)} KB</span>
                    <span>{new Date(result.indexed_at).toLocaleDateString()}</span>
                  </div>

                  <a
                    href={`/repositories/${result.repository_id}?path=${encodeURIComponent(result.file_path)}`}
                    className="text-xs text-blue-400 hover:text-blue-300 flex items-center space-x-1"
                  >
                    <span>View in repository</span>
                    <svg className="w-3 h-3" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
                    </svg>
                  </a>
                </div>

                {/* Expand/collapse button */}
                <button
                  onClick={() => toggleExpanded(resultId)}
                  className="mt-3 text-sm text-blue-400 hover:text-blue-300 flex items-center space-x-1"
                >
                  <span>{isExpanded ? 'Hide' : 'Show'} file content</span>
                  <svg
                    className={`w-4 h-4 transition-transform ${isExpanded ? 'rotate-180' : ''}`}
                    fill="none"
                    viewBox="0 0 24 24"
                    stroke="currentColor"
                  >
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 9l-7 7-7-7" />
                  </svg>
                </button>
              </div>

              {/* Expanded code preview */}
              {isExpanded && (
                <div className="border-t border-gray-700 p-4">
                  <CodePreview
                    repositoryId={result.repository_id}
                    filePath={result.file_path}
                    language={result.language}
                    highlights={result.highlights}
                    maxLines={50}
                  />
                </div>
              )}
            </div>
          )
        })}
      </div>

      {/* Pagination */}
      {totalPages > 1 && (
        <div className="flex items-center justify-between border-t border-gray-700 pt-4">
          <button
            onClick={onPrevPage}
            disabled={page === 1}
            className="px-4 py-2 text-sm font-medium text-gray-300 bg-gray-800 border border-gray-700 rounded-lg hover:bg-gray-700 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            Previous
          </button>

          <div className="flex items-center space-x-2">
            {/* Page numbers */}
            {Array.from({ length: Math.min(5, totalPages) }, (_, i) => {
              let pageNum: number
              if (totalPages <= 5) {
                pageNum = i + 1
              } else if (page <= 3) {
                pageNum = i + 1
              } else if (page >= totalPages - 2) {
                pageNum = totalPages - 4 + i
              } else {
                pageNum = page - 2 + i
              }

              return (
                <button
                  key={pageNum}
                  onClick={() => onGoToPage(pageNum)}
                  className={`px-3 py-1 text-sm font-medium rounded ${
                    page === pageNum
                      ? 'bg-blue-600 text-white'
                      : 'text-gray-300 hover:bg-gray-700'
                  }`}
                >
                  {pageNum}
                </button>
              )
            })}
          </div>

          <button
            onClick={onNextPage}
            disabled={page === totalPages}
            className="px-4 py-2 text-sm font-medium text-gray-300 bg-gray-800 border border-gray-700 rounded-lg hover:bg-gray-700 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            Next
          </button>
        </div>
      )}
    </div>
  )
}
