'use client'

import { Box, Braces, Code2,Filter, FunctionSquare, Package, Search, X } from 'lucide-react'
import React, { useState } from 'react'

import { SymbolSearchResults } from '@/components/search/SymbolSearchResults'
import { useSymbolSearch } from '@/lib/hooks/use-symbol-search'

const SYMBOL_KINDS = [
  { value: 'function', label: 'Functions', icon: FunctionSquare, color: 'blue' },
  { value: 'class', label: 'Classes', icon: Box, color: 'purple' },
  { value: 'method', label: 'Methods', icon: Code2, color: 'green' },
  { value: 'variable', label: 'Variables', icon: Braces, color: 'yellow' },
  { value: 'import', label: 'Imports', icon: Package, color: 'gray' },
]

const LANGUAGES = [
  'Python',
  'JavaScript',
  'TypeScript',
  'Go',
  'Rust',
  'Java',
  'C',
  'C++',
  'C#',
  'Ruby',
  'PHP',
  'Swift',
  'Kotlin',
]

export default function SymbolSearchPage() {
  const [localQuery, setLocalQuery] = useState('')
  const [showFilters, setShowFilters] = useState(true)

  const {
    query,
    setQuery,
    results,
    total,
    page,
    totalPages,
    isLoading,
    error,
    kind,
    language,
    filePath,
    search,
    nextPage,
    prevPage,
    goToPage,
    setKind,
    setLanguage,
    setFilePath,
    clearFilters,
  } = useSymbolSearch({
    autoSearch: false,
    per_page: 20,
    debounceMs: 300,
  })

  const handleSearch = (e: React.FormEvent) => {
    e.preventDefault()
    setQuery(localQuery)
    search(localQuery)
  }

  const handleKindToggle = (kindValue: string) => {
    if (kind === kindValue) {
      setKind(undefined)
    } else {
      setKind(kindValue)
    }
  }

  const handleLanguageSelect = (lang: string) => {
    if (language === lang) {
      setLanguage(undefined)
    } else {
      setLanguage(lang)
    }
  }

  const activeFiltersCount =
    (kind ? 1 : 0) + (language ? 1 : 0) + (filePath ? 1 : 0)

  // Calculate time estimate
  const estimatedTime = total > 0 ? (total < 100 ? '< 100ms' : '< 500ms') : '—'

  return (
    <div className="min-h-screen bg-gray-950">
      {/* Header */}
      <div className="border-b border-gray-800 bg-gray-900/50 backdrop-blur">
        <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 py-6">
          <div className="flex items-center justify-between mb-6">
            <div className="flex items-center space-x-3">
              <Code2 className="h-8 w-8 text-blue-500" />
              <h1 className="text-3xl font-bold text-white">Symbol Search</h1>
            </div>
            <div className="text-sm text-gray-500">
              Powered by Elasticsearch + SCIP
            </div>
          </div>

          {/* Search form */}
          <form onSubmit={handleSearch} className="space-y-4">
            <div className="flex space-x-3">
              <div className="flex-1 relative">
                <Search className="absolute left-4 top-1/2 transform -translate-y-1/2 h-5 w-5 text-gray-500 pointer-events-none" />
                <input
                  type="text"
                  value={localQuery}
                  onChange={(e) => setLocalQuery(e.target.value)}
                  placeholder="Search for functions, classes, methods..."
                  className="w-full pl-12 pr-4 py-3 bg-gray-800 border border-gray-700 rounded-lg text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                />
              </div>
              <button
                type="submit"
                disabled={!localQuery.trim() || isLoading}
                className="px-6 py-3 bg-blue-600 text-white rounded-lg hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 focus:ring-offset-gray-900 disabled:opacity-50 disabled:cursor-not-allowed font-medium transition-colors"
              >
                {isLoading ? 'Searching...' : 'Search'}
              </button>
              <button
                type="button"
                onClick={() => setShowFilters(!showFilters)}
                className={`px-4 py-3 rounded-lg border transition-colors ${
                  showFilters
                    ? 'bg-blue-600 border-blue-600 text-white'
                    : 'bg-gray-800 border-gray-700 text-gray-300 hover:bg-gray-700'
                }`}
                title="Toggle filters"
              >
                <Filter className="h-5 w-5" />
              </button>
            </div>

            {/* File path filter - always visible */}
            <div>
              <input
                type="text"
                value={filePath || ''}
                onChange={(e) => setFilePath(e.target.value || undefined)}
                placeholder="Filter by file path (e.g., src/components)"
                className="w-full px-4 py-2 bg-gray-800 border border-gray-700 rounded-lg text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent text-sm"
              />
            </div>
          </form>

          {/* Quick stats */}
          {total > 0 && (
            <div className="mt-4 flex items-center space-x-6 text-sm">
              <div className="text-gray-400">
                <span className="font-medium text-white">{total.toLocaleString()}</span> symbols
              </div>
              <div className="text-gray-400">
                Page <span className="font-medium text-white">{page}</span> of{' '}
                <span className="font-medium text-white">{totalPages}</span>
              </div>
              <div className="text-gray-400">
                Est. time: <span className="font-medium text-white">{estimatedTime}</span>
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Main content */}
      <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 py-8">
        {showFilters && (
          <div className="mb-8 space-y-6">
            {/* Symbol kinds filter */}
            <div>
              <div className="flex items-center justify-between mb-3">
                <h2 className="text-sm font-semibold text-gray-300 uppercase tracking-wide">
                  Symbol Type
                </h2>
                {kind && (
                  <button
                    onClick={() => setKind(undefined)}
                    className="text-xs text-blue-400 hover:text-blue-300 flex items-center space-x-1"
                  >
                    <X className="h-3 w-3" />
                    <span>Clear</span>
                  </button>
                )}
              </div>
              <div className="flex flex-wrap gap-2">
                {SYMBOL_KINDS.map((symbolKind) => {
                  const Icon = symbolKind.icon
                  const isActive = kind === symbolKind.value
                  return (
                    <button
                      key={symbolKind.value}
                      onClick={() => handleKindToggle(symbolKind.value)}
                      className={`flex items-center space-x-2 px-4 py-2 rounded-lg border transition-all ${
                        isActive
                          ? 'bg-blue-600 border-blue-600 text-white shadow-lg shadow-blue-500/20'
                          : 'bg-gray-800 border-gray-700 text-gray-300 hover:bg-gray-700 hover:border-gray-600'
                      }`}
                    >
                      <Icon className="h-4 w-4" />
                      <span className="text-sm font-medium">{symbolKind.label}</span>
                    </button>
                  )
                })}
              </div>
            </div>

            {/* Languages filter */}
            <div>
              <div className="flex items-center justify-between mb-3">
                <h2 className="text-sm font-semibold text-gray-300 uppercase tracking-wide">
                  Language
                </h2>
                {language && (
                  <button
                    onClick={() => setLanguage(undefined)}
                    className="text-xs text-blue-400 hover:text-blue-300 flex items-center space-x-1"
                  >
                    <X className="h-3 w-3" />
                    <span>Clear</span>
                  </button>
                )}
              </div>
              <div className="flex flex-wrap gap-2">
                {LANGUAGES.map((lang) => {
                  const isActive = language === lang.toLowerCase()
                  return (
                    <button
                      key={lang}
                      onClick={() => handleLanguageSelect(lang.toLowerCase())}
                      className={`px-3 py-1.5 rounded-lg text-sm font-medium border transition-all ${
                        isActive
                          ? 'bg-purple-600 border-purple-600 text-white shadow-lg shadow-purple-500/20'
                          : 'bg-gray-800 border-gray-700 text-gray-300 hover:bg-gray-700 hover:border-gray-600'
                      }`}
                    >
                      {lang}
                    </button>
                  )
                })}
              </div>
            </div>

            {/* Clear all filters */}
            {activeFiltersCount > 0 && (
              <div className="pt-2">
                <button
                  onClick={clearFilters}
                  className="text-sm text-red-400 hover:text-red-300 flex items-center space-x-1"
                >
                  <X className="h-4 w-4" />
                  <span>Clear all filters ({activeFiltersCount})</span>
                </button>
              </div>
            )}
          </div>
        )}

        {/* Results */}
        <SymbolSearchResults
          results={results}
          query={query}
          isLoading={isLoading}
          error={error}
          total={total}
          page={page}
          totalPages={totalPages}
          tookMs={0} // Not available in current API
          onNextPage={nextPage}
          onPrevPage={prevPage}
          onGoToPage={goToPage}
        />
      </div>
    </div>
  )
}
