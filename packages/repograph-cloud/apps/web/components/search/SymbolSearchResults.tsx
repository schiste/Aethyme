'use client'

import { Box, Braces, Code2, FileCode, Function, Package } from 'lucide-react'
import React, { useState } from 'react'

interface SymbolResult {
  symbol_id: string
  kind: string // function, class, method, variable, import
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

interface SymbolSearchResultsProps {
  results: SymbolResult[]
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

const SYMBOL_ICONS: Record<string, React.ElementType> = {
  function: Function,
  class: Box,
  method: Code2,
  variable: Braces,
  import: Package,
  default: FileCode,
}

const SYMBOL_COLORS: Record<string, string> = {
  function: 'text-blue-400 bg-blue-950 border-blue-800',
  class: 'text-purple-400 bg-purple-950 border-purple-800',
  method: 'text-green-400 bg-green-950 border-green-800',
  variable: 'text-yellow-400 bg-yellow-950 border-yellow-800',
  import: 'text-gray-400 bg-gray-800 border-gray-700',
  default: 'text-gray-400 bg-gray-800 border-gray-700',
}

function getSymbolIcon(kind: string) {
  return SYMBOL_ICONS[kind] || SYMBOL_ICONS.default
}

function getSymbolColor(kind: string) {
  return SYMBOL_COLORS[kind] || SYMBOL_COLORS.default
}

export function SymbolSearchResults({
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
  onGoToPage,
}: SymbolSearchResultsProps) {
  const [expandedSymbols, setExpandedSymbols] = useState<Set<string>>(new Set())

  const toggleExpanded = (symbolId: string) => {
    setExpandedSymbols((prev) => {
      const next = new Set(prev)
      if (next.has(symbolId)) {
        next.delete(symbolId)
      } else {
        next.add(symbolId)
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
      <div className="space-y-3">
        {[...Array(8)].map((_, i) => (
          <div
            key={i}
            className="border border-gray-700 rounded-lg p-4 bg-gray-800/50 animate-pulse"
          >
            <div className="flex items-start space-x-3">
              <div className="w-8 h-8 bg-gray-700 rounded"></div>
              <div className="flex-1 space-y-2">
                <div className="h-5 bg-gray-700 rounded w-1/3"></div>
                <div className="h-4 bg-gray-700 rounded w-2/3"></div>
              </div>
            </div>
          </div>
        ))}
      </div>
    )
  }

  if (!query) {
    return (
      <div className="text-center py-16">
        <Code2 className="mx-auto h-16 w-16 text-gray-600 mb-4" />
        <h3 className="text-xl font-medium text-gray-300 mb-2">Search Your Code Symbols</h3>
        <p className="text-sm text-gray-500 max-w-md mx-auto">
          Find functions, classes, methods, and variables across all your repositories with semantic
          search
        </p>
      </div>
    )
  }

  if (results.length === 0) {
    return (
      <div className="text-center py-16">
        <FileCode className="mx-auto h-16 w-16 text-gray-600 mb-4" />
        <h3 className="text-xl font-medium text-gray-300 mb-2">No symbols found</h3>
        <p className="text-sm text-gray-500 max-w-md mx-auto">
          Try adjusting your search query or filters. Use broader terms or remove filters to see more
          results.
        </p>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Results header */}
      <div className="flex items-center justify-between text-sm">
        <div className="text-gray-400">
          <span className="font-medium text-white">{total.toLocaleString()}</span>{' '}
          {total === 1 ? 'symbol' : 'symbols'} found
          {tookMs > 0 && (
            <span className="ml-2 text-gray-600">({tookMs.toFixed(0)}ms)</span>
          )}
        </div>
        <div className="text-gray-500">
          Page {page} of {totalPages}
        </div>
      </div>

      {/* Results list */}
      <div className="space-y-2">
        {results.map((result) => {
          const Icon = getSymbolIcon(result.kind)
          const colorClass = getSymbolColor(result.kind)
          const isExpanded = expandedSymbols.has(result.symbol_id)

          return (
            <div
              key={result.symbol_id}
              className="border border-gray-700 rounded-lg bg-gray-800/50 hover:border-gray-600 hover:bg-gray-800 transition-all"
            >
              <div className="p-4">
                <div className="flex items-start space-x-3">
                  {/* Symbol icon */}
                  <div className={`flex-shrink-0 p-2 rounded-lg border ${colorClass}`}>
                    <Icon className="h-4 w-4" />
                  </div>

                  {/* Symbol info */}
                  <div className="flex-1 min-w-0">
                    {/* Symbol name and kind */}
                    <div className="flex items-center space-x-2 mb-1">
                      <h3 className="text-base font-mono font-semibold text-white truncate">
                        {result.name}
                      </h3>
                      <span className="flex-shrink-0 text-xs uppercase px-2 py-0.5 rounded bg-gray-700 text-gray-400">
                        {result.kind}
                      </span>
                      <span className="flex-shrink-0 text-xs uppercase px-2 py-0.5 rounded bg-gray-700 text-gray-400">
                        {result.language}
                      </span>
                    </div>

                    {/* Signature */}
                    {result.signature && (
                      <div className="text-sm font-mono text-gray-400 mb-2 truncate">
                        {result.signature}
                      </div>
                    )}

                    {/* Documentation preview */}
                    {result.documentation && (
                      <p className="text-sm text-gray-500 mb-2 line-clamp-2">
                        {result.documentation}
                      </p>
                    )}

                    {/* Location */}
                    <div className="flex items-center space-x-3 text-xs text-gray-500">
                      <span className="flex items-center space-x-1">
                        <FileCode className="h-3 w-3" />
                        <span className="font-mono">{result.file_path}</span>
                      </span>
                      <span>Line {result.line_start}</span>
                      <span className="text-gray-600">•</span>
                      <span className="truncate max-w-xs">{result.repository_name}</span>
                    </div>
                  </div>

                  {/* Score badge */}
                  <div className="flex-shrink-0 text-right">
                    <div className="text-xs text-gray-500">
                      Score: <span className="font-mono">{result.score.toFixed(2)}</span>
                    </div>
                  </div>
                </div>

                {/* Expanded details */}
                {isExpanded && (
                  <div className="mt-4 pt-4 border-t border-gray-700 space-y-3">
                    {/* Full signature */}
                    {result.signature && (
                      <div>
                        <h4 className="text-xs font-semibold text-gray-400 mb-1">Signature</h4>
                        <pre className="text-sm font-mono text-gray-300 bg-gray-900 rounded p-3 overflow-x-auto">
                          {result.signature}
                        </pre>
                      </div>
                    )}

                    {/* Full documentation */}
                    {result.documentation && (
                      <div>
                        <h4 className="text-xs font-semibold text-gray-400 mb-1">Documentation</h4>
                        <div className="text-sm text-gray-300 bg-gray-900 rounded p-3">
                          {result.documentation}
                        </div>
                      </div>
                    )}

                    {/* Symbol ID */}
                    <div>
                      <h4 className="text-xs font-semibold text-gray-400 mb-1">Symbol ID</h4>
                      <code className="text-xs font-mono text-gray-500 bg-gray-900 rounded px-2 py-1">
                        {result.symbol_id}
                      </code>
                    </div>

                    {/* Actions */}
                    <div className="flex items-center space-x-3 pt-2">
                      <a
                        href={`/repositories/${result.repository_name}/files/${result.file_path}#L${result.line_start}`}
                        className="text-sm text-blue-400 hover:text-blue-300 flex items-center space-x-1"
                      >
                        <Code2 className="h-4 w-4" />
                        <span>View in file</span>
                      </a>
                      <button
                        onClick={() => navigator.clipboard.writeText(result.symbol_id)}
                        className="text-sm text-gray-400 hover:text-gray-300 flex items-center space-x-1"
                      >
                        <svg
                          className="h-4 w-4"
                          fill="none"
                          viewBox="0 0 24 24"
                          stroke="currentColor"
                        >
                          <path
                            strokeLinecap="round"
                            strokeLinejoin="round"
                            strokeWidth={2}
                            d="M8 5H6a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2v-1M8 5a2 2 0 002 2h2a2 2 0 002-2M8 5a2 2 0 012-2h2a2 2 0 012 2m0 0h2a2 2 0 012 2v3m2 4H10m0 0l3-3m-3 3l3 3"
                          />
                        </svg>
                        <span>Copy ID</span>
                      </button>
                    </div>
                  </div>
                )}

                {/* Toggle button */}
                <button
                  onClick={() => toggleExpanded(result.symbol_id)}
                  className="mt-3 text-xs text-blue-400 hover:text-blue-300 flex items-center space-x-1"
                >
                  <span>{isExpanded ? 'Show less' : 'Show more'}</span>
                  <svg
                    className={`w-4 h-4 transition-transform ${isExpanded ? 'rotate-180' : ''}`}
                    fill="none"
                    viewBox="0 0 24 24"
                    stroke="currentColor"
                  >
                    <path
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      strokeWidth={2}
                      d="M19 9l-7 7-7-7"
                    />
                  </svg>
                </button>
              </div>
            </div>
          )
        })}
      </div>

      {/* Pagination */}
      {totalPages > 1 && (
        <div className="flex items-center justify-between border-t border-gray-700 pt-6 mt-6">
          <button
            onClick={onPrevPage}
            disabled={page === 1}
            className="px-4 py-2 text-sm font-medium text-gray-300 bg-gray-800 border border-gray-700 rounded-lg hover:bg-gray-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            Previous
          </button>

          <div className="flex items-center space-x-2">
            {Array.from({ length: Math.min(7, totalPages) }, (_, i) => {
              let pageNum: number
              if (totalPages <= 7) {
                pageNum = i + 1
              } else if (page <= 4) {
                pageNum = i + 1
              } else if (page >= totalPages - 3) {
                pageNum = totalPages - 6 + i
              } else {
                pageNum = page - 3 + i
              }

              return (
                <button
                  key={pageNum}
                  onClick={() => onGoToPage(pageNum)}
                  className={`px-3 py-1 text-sm font-medium rounded transition-colors ${
                    page === pageNum
                      ? 'bg-blue-600 text-white'
                      : 'text-gray-400 hover:bg-gray-700 hover:text-gray-300'
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
            className="px-4 py-2 text-sm font-medium text-gray-300 bg-gray-800 border border-gray-700 rounded-lg hover:bg-gray-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            Next
          </button>
        </div>
      )}
    </div>
  )
}
