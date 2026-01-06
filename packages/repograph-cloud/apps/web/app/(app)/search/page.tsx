"use client"

import { Input, Select } from '@aeptus/ui'
import { ChevronLeft, ChevronRight, FileCode, Filter, Loader2,Search } from 'lucide-react'
import { useCallback, useEffect,useState } from 'react'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'

interface SearchResult {
  symbol: {
    id: number
    name: string
    kind: string
    signature: string | null
    docstring: string | null
    file_path: string
    line_number: number | null
    language: string | null
  }
  score: number
  highlights: {
    field: string
    snippet: string
  }[]
}

interface SearchResponse {
  results: SearchResult[]
  total: number
  page: number
  page_size: number
  total_pages: number
  facets: {
    languages: { [key: string]: number }
    kinds: { [key: string]: number }
    files: { [key: string]: number }
  }
  query_info: {
    query: string
    mode: string
    scope: string
    execution_time_ms: number
  }
}

export default function SearchPage() {
  const [query, setQuery] = useState("")
  const [results, setResults] = useState<SearchResult[]>([])
  const [facets, setFacets] = useState<SearchResponse['facets']>({
    languages: {},
    kinds: {},
    files: {}
  })
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  
  // Search parameters
  const [searchMode, setSearchMode] = useState("contains")
  const [searchScope, setSearchScope] = useState("all")
  const [languageFilter, setLanguageFilter] = useState("")
  const [kindFilter, setKindFilter] = useState("")
  const [page, setPage] = useState(1)
  const [totalPages, setTotalPages] = useState(1)
  const [total, setTotal] = useState(0)
  const [executionTime, setExecutionTime] = useState(0)
  
  const [showFilters, setShowFilters] = useState(false)

  const executeSearch = useCallback(async () => {
    if (!query.trim()) {
      setResults([])
      setTotal(0)
      setTotalPages(1)
      return
    }

    setLoading(true)
    setError(null)

    try {
      const params = new URLSearchParams({
        q: query,
        mode: searchMode,
        scope: searchScope,
        page: page.toString(),
        page_size: "20"
      })

      if (languageFilter) params.append("language", languageFilter)
      if (kindFilter) params.append("kind", kindFilter)

      const token = localStorage.getItem("access_token")
      const response = await fetch(`/api/v1/search/symbols?${params}`, {
        headers: {
          "Authorization": `Bearer ${token}`,
          "Content-Type": "application/json"
        }
      })

      if (!response.ok) {
        throw new Error(`Search failed: ${response.statusText}`)
      }

      const data: SearchResponse = await response.json()
      setResults(data.results)
      setFacets(data.facets)
      setTotal(data.total)
      setTotalPages(data.total_pages)
      setExecutionTime(data.query_info.execution_time_ms)
    } catch (err) {
      setError(err instanceof Error ? err.message : "Search failed")
      setResults([])
    } finally {
      setLoading(false)
    }
  }, [query, searchMode, searchScope, languageFilter, kindFilter, page])

  // Execute search on parameter change
  useEffect(() => {
    const debounce = setTimeout(() => {
      executeSearch()
    }, 300)

    return () => clearTimeout(debounce)
  }, [executeSearch])

  const handleKeyPress = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      executeSearch()
    }
  }

  const resetFilters = () => {
    setSearchMode("contains")
    setSearchScope("all")
    setLanguageFilter("")
    setKindFilter("")
    setPage(1)
  }

  const getKindIcon = (kind: string) => {
    switch (kind.toLowerCase()) {
      case 'class':
        return '🏛️'
      case 'function':
      case 'method':
        return '⚡'
      case 'variable':
      case 'field':
        return '📦'
      case 'interface':
      case 'trait':
        return '🔌'
      case 'module':
      case 'package':
        return '📁'
      default:
        return '📄'
    }
  }

  const getKindColor = (kind: string) => {
    switch (kind.toLowerCase()) {
      case 'class':
        return 'bg-blue-500/10 text-blue-700 dark:text-blue-400'
      case 'function':
      case 'method':
        return 'bg-purple-500/10 text-purple-700 dark:text-purple-400'
      case 'variable':
      case 'field':
        return 'bg-green-500/10 text-green-700 dark:text-green-400'
      case 'interface':
      case 'trait':
        return 'bg-orange-500/10 text-orange-700 dark:text-orange-400'
      default:
        return 'bg-slate-500/10 text-slate-700 dark:text-slate-400'
    }
  }

  return (
    <div className="container mx-auto p-6 max-w-7xl">
      <div className="mb-8">
        <h1 className="text-3xl font-bold text-slate-900 dark:text-white mb-2">
          Advanced Code Search
        </h1>
        <p className="text-slate-600 dark:text-slate-400">
          Search across all indexed symbols with powerful filters and modes
        </p>
      </div>

      {/* Search Bar */}
      <div className="mb-6">
        <div className="flex gap-2">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-5 w-5 text-slate-400" />
            <Input
              type="text"
              placeholder="Search for classes, functions, variables..."
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyPress={handleKeyPress}
              className="pl-10 h-12 text-lg"
            />
          </div>
          <Button
            variant="outline"
            size="icon"
            onClick={() => setShowFilters(!showFilters)}
            className="h-12 w-12"
          >
            <Filter className="h-5 w-5" />
          </Button>
          <Button
            onClick={executeSearch}
            disabled={loading || !query.trim()}
            className="h-12 px-6"
          >
            {loading ? (
              <>
                <Loader2 className="mr-2 h-5 w-5 animate-spin" />
                Searching...
              </>
            ) : (
              <>
                <Search className="mr-2 h-5 w-5" />
                Search
              </>
            )}
          </Button>
        </div>
      </div>

      {/* Filters Panel */}
      {showFilters && (
        <Card className="mb-6">
          <CardHeader>
            <div className="flex items-center justify-between">
              <CardTitle>Search Filters</CardTitle>
              <Button variant="ghost" size="sm" onClick={resetFilters}>
                Reset All
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
              <div>
                <label className="text-sm font-medium mb-2 block">Search Mode</label>
                <Select value={searchMode} onValueChange={setSearchMode}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="exact">Exact Match</SelectItem>
                    <SelectItem value="contains">Contains</SelectItem>
                    <SelectItem value="prefix">Starts With</SelectItem>
                    <SelectItem value="fuzzy">Fuzzy Match</SelectItem>
                    <SelectItem value="regex">Regex</SelectItem>
                  </SelectContent>
                </Select>
              </div>

              <div>
                <label className="text-sm font-medium mb-2 block">Search Scope</label>
                <Select value={searchScope} onValueChange={setSearchScope}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All Symbols</SelectItem>
                    <SelectItem value="name">Name Only</SelectItem>
                    <SelectItem value="signature">Signature</SelectItem>
                    <SelectItem value="docstring">Documentation</SelectItem>
                    <SelectItem value="file">File Path</SelectItem>
                  </SelectContent>
                </Select>
              </div>

              <div>
                <label className="text-sm font-medium mb-2 block">Language</label>
                <Select value={languageFilter} onValueChange={setLanguageFilter}>
                  <SelectTrigger>
                    <SelectValue placeholder="All Languages" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="">All Languages</SelectItem>
                    <SelectItem value="python">Python</SelectItem>
                    <SelectItem value="javascript">JavaScript</SelectItem>
                    <SelectItem value="typescript">TypeScript</SelectItem>
                    <SelectItem value="java">Java</SelectItem>
                    <SelectItem value="ruby">Ruby</SelectItem>
                    <SelectItem value="rust">Rust</SelectItem>
                    <SelectItem value="go">Go</SelectItem>
                  </SelectContent>
                </Select>
              </div>

              <div>
                <label className="text-sm font-medium mb-2 block">Symbol Kind</label>
                <Select value={kindFilter} onValueChange={setKindFilter}>
                  <SelectTrigger>
                    <SelectValue placeholder="All Kinds" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="">All Kinds</SelectItem>
                    <SelectItem value="class">Class</SelectItem>
                    <SelectItem value="function">Function</SelectItem>
                    <SelectItem value="method">Method</SelectItem>
                    <SelectItem value="variable">Variable</SelectItem>
                    <SelectItem value="interface">Interface</SelectItem>
                    <SelectItem value="module">Module</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Results Summary */}
      {query && (
        <div className="mb-4 flex items-center justify-between">
          <p className="text-sm text-slate-600 dark:text-slate-400">
            {loading ? (
              "Searching..."
            ) : (
              <>
                Found <strong>{total}</strong> results in <strong>{executionTime}ms</strong>
              </>
            )}
          </p>
          {total > 0 && (
            <p className="text-sm text-slate-600 dark:text-slate-400">
              Page {page} of {totalPages}
            </p>
          )}
        </div>
      )}

      {/* Error State */}
      {error && (
        <Card className="border-red-200 bg-red-50 dark:bg-red-950/20 dark:border-red-900 mb-6">
          <CardContent className="pt-6">
            <p className="text-red-700 dark:text-red-400">{error}</p>
          </CardContent>
        </Card>
      )}

      <div className="grid grid-cols-1 lg:grid-cols-4 gap-6">
        {/* Facets Sidebar */}
        {results.length > 0 && (
          <div className="lg:col-span-1">
            <Card>
              <CardHeader>
                <CardTitle className="text-lg">Refine Results</CardTitle>
              </CardHeader>
              <CardContent className="space-y-4">
                {/* Language Facets */}
                {Object.keys(facets.languages).length > 0 && (
                  <div>
                    <h4 className="text-sm font-semibold mb-2">Languages</h4>
                    <div className="space-y-1">
                      {Object.entries(facets.languages)
                        .sort(([, a], [, b]) => b - a)
                        .slice(0, 5)
                        .map(([lang, count]) => (
                          <button
                            key={lang}
                            onClick={() => setLanguageFilter(lang === languageFilter ? "" : lang)}
                            className={`w-full text-left px-2 py-1 rounded text-sm hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors ${
                              lang === languageFilter ? 'bg-slate-100 dark:bg-slate-800' : ''
                            }`}
                          >
                            <span className="capitalize">{lang}</span>
                            <span className="float-right text-slate-500">{count}</span>
                          </button>
                        ))}
                    </div>
                  </div>
                )}

                <Separator />

                {/* Kind Facets */}
                {Object.keys(facets.kinds).length > 0 && (
                  <div>
                    <h4 className="text-sm font-semibold mb-2">Symbol Types</h4>
                    <div className="space-y-1">
                      {Object.entries(facets.kinds)
                        .sort(([, a], [, b]) => b - a)
                        .map(([kind, count]) => (
                          <button
                            key={kind}
                            onClick={() => setKindFilter(kind === kindFilter ? "" : kind)}
                            className={`w-full text-left px-2 py-1 rounded text-sm hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors ${
                              kind === kindFilter ? 'bg-slate-100 dark:bg-slate-800' : ''
                            }`}
                          >
                            <span className="capitalize">{kind}</span>
                            <span className="float-right text-slate-500">{count}</span>
                          </button>
                        ))}
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          </div>
        )}

        {/* Results List */}
        <div className={results.length > 0 ? "lg:col-span-3" : "lg:col-span-4"}>
          {loading && results.length === 0 ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="h-8 w-8 animate-spin text-slate-400" />
            </div>
          ) : results.length === 0 && query ? (
            <Card>
              <CardContent className="pt-6 text-center py-12">
                <FileCode className="h-12 w-12 text-slate-300 dark:text-slate-700 mx-auto mb-4" />
                <h3 className="text-lg font-semibold text-slate-900 dark:text-white mb-2">
                  No results found
                </h3>
                <p className="text-slate-600 dark:text-slate-400 mb-4">
                  Try adjusting your search query or filters
                </p>
                <Button variant="outline" onClick={resetFilters}>
                  Reset Filters
                </Button>
              </CardContent>
            </Card>
          ) : results.length === 0 ? (
            <Card>
              <CardContent className="pt-6 text-center py-12">
                <Search className="h-12 w-12 text-slate-300 dark:text-slate-700 mx-auto mb-4" />
                <h3 className="text-lg font-semibold text-slate-900 dark:text-white mb-2">
                  Start searching
                </h3>
                <p className="text-slate-600 dark:text-slate-400">
                  Enter a search query to find symbols across your repositories
                </p>
              </CardContent>
            </Card>
          ) : (
            <div className="space-y-4">
              {results.map((result) => (
                <Card key={result.symbol.id} className="hover:border-slate-400 transition-colors">
                  <CardHeader>
                    <div className="flex items-start justify-between gap-4">
                      <div className="flex-1">
                        <div className="flex items-center gap-2 mb-2">
                          <span className="text-xl">{getKindIcon(result.symbol.kind)}</span>
                          <CardTitle className="text-xl font-mono">
                            {result.symbol.name}
                          </CardTitle>
                          <Badge className={getKindColor(result.symbol.kind)}>
                            {result.symbol.kind}
                          </Badge>
                          {result.symbol.language && (
                            <Badge variant="outline" className="capitalize">
                              {result.symbol.language}
                            </Badge>
                          )}
                        </div>
                        {result.symbol.signature && (
                          <code className="text-sm text-slate-600 dark:text-slate-400 block mb-2">
                            {result.symbol.signature}
                          </code>
                        )}
                      </div>
                      <div className="text-right">
                        <div className="text-sm font-semibold text-slate-900 dark:text-white">
                          {result.score.toFixed(0)}%
                        </div>
                        <div className="text-xs text-slate-500">relevance</div>
                      </div>
                    </div>
                    {result.symbol.docstring && (
                      <CardDescription className="mt-2">
                        {result.symbol.docstring.slice(0, 200)}
                        {result.symbol.docstring.length > 200 && '...'}
                      </CardDescription>
                    )}
                  </CardHeader>
                  <CardContent>
                    <div className="flex items-center gap-2 text-sm text-slate-600 dark:text-slate-400">
                      <FileCode className="h-4 w-4" />
                      <code className="flex-1">{result.symbol.file_path}</code>
                      {result.symbol.line_number && (
                        <span className="text-slate-500">Line {result.symbol.line_number}</span>
                      )}
                    </div>
                    {result.highlights.length > 0 && (
                      <div className="mt-3 p-3 bg-slate-50 dark:bg-slate-900 rounded text-sm">
                        {result.highlights.map((highlight, idx) => (
                          <div key={idx} className="mb-1 last:mb-0">
                            <span className="font-semibold text-slate-700 dark:text-slate-300">
                              {highlight.field}:
                            </span>{' '}
                            <span className="text-slate-600 dark:text-slate-400">
                              {highlight.snippet}
                            </span>
                          </div>
                        ))}
                      </div>
                    )}
                  </CardContent>
                </Card>
              ))}

              {/* Pagination */}
              {totalPages > 1 && (
                <div className="flex items-center justify-center gap-2 pt-4">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setPage(Math.max(1, page - 1))}
                    disabled={page === 1 || loading}
                  >
                    <ChevronLeft className="h-4 w-4 mr-1" />
                    Previous
                  </Button>
                  <div className="flex items-center gap-1">
                    {Array.from({ length: Math.min(5, totalPages) }, (_, i) => {
                      const pageNum = i + 1
                      return (
                        <Button
                          key={pageNum}
                          variant={page === pageNum ? "default" : "outline"}
                          size="sm"
                          onClick={() => setPage(pageNum)}
                          disabled={loading}
                        >
                          {pageNum}
                        </Button>
                      )
                    })}
                    {totalPages > 5 && (
                      <>
                        <span className="px-2">...</span>
                        <Button
                          variant={page === totalPages ? "default" : "outline"}
                          size="sm"
                          onClick={() => setPage(totalPages)}
                          disabled={loading}
                        >
                          {totalPages}
                        </Button>
                      </>
                    )}
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setPage(Math.min(totalPages, page + 1))}
                    disabled={page === totalPages || loading}
                  >
                    Next
                    <ChevronRight className="h-4 w-4 ml-1" />
                  </Button>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
