'use client'

import { Search, Sparkles } from 'lucide-react'
import { useState } from 'react'

import { ExampleQueries } from '@/components/ai/ExampleQueries'
import { SemanticSearchFilters } from '@/components/ai/SemanticSearchFilters'
import { SemanticSearchResults } from '@/components/ai/SemanticSearchResults'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Textarea } from '@/components/ui/textarea'

export default function SemanticSearchPage() {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<any>(null)
  const [loading, setLoading] = useState(false)
  const [filters, setFilters] = useState({
    repository_id: '',
    language: '',
    symbol_kind: '',
    limit: 10,
  })

  const handleSearch = async () => {
    if (!query.trim()) return

    setLoading(true)
    try {
      const response = await fetch('/api/v1/semantic/search', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${localStorage.getItem('token')}`,
        },
        body: JSON.stringify({
          query: query.trim(),
          ...filters,
        }),
      })

      if (!response.ok) {
        throw new Error('Search failed')
      }

      const data = await response.json()
      setResults(data)
    } catch (error) {
      console.error('Search error:', error)
    } finally {
      setLoading(false)
    }
  }

  const handleExampleClick = (exampleQuery: string) => {
    setQuery(exampleQuery)
  }

  return (
    <div className="space-y-6">
      <div>
        <div className="flex items-center gap-2 mb-2">
          <Sparkles className="h-6 w-6 text-purple-500" />
          <h1 className="text-3xl font-bold tracking-tight">Semantic Search</h1>
        </div>
        <p className="text-muted-foreground">
          Use natural language to find code across your repositories
        </p>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        <div className="lg:col-span-2 space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>Search Query</CardTitle>
              <CardDescription>
                Describe what you're looking for in plain English
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <Textarea
                placeholder="e.g., function that validates email addresses&#10;e.g., API endpoint for user authentication&#10;e.g., class that handles database connections"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                className="min-h-[120px] resize-none"
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                    handleSearch()
                  }
                }}
              />
              <div className="flex items-center justify-between">
                <p className="text-xs text-muted-foreground">
                  Press ⌘+Enter to search
                </p>
                <Button onClick={handleSearch} disabled={loading || !query.trim()}>
                  <Search className="h-4 w-4 mr-2" />
                  {loading ? 'Searching...' : 'Search'}
                </Button>
              </div>
            </CardContent>
          </Card>

          <SemanticSearchFilters filters={filters} onFiltersChange={setFilters} />

          {results && <SemanticSearchResults results={results} query={query} />}
        </div>

        <div className="space-y-6">
          <ExampleQueries onExampleClick={handleExampleClick} />

          <Card>
            <CardHeader>
              <CardTitle className="text-sm">How It Works</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3 text-sm text-muted-foreground">
              <div>
                <p className="font-semibold text-foreground mb-1">1. Natural Language</p>
                <p>Describe what you're looking for in plain English</p>
              </div>
              <div>
                <p className="font-semibold text-foreground mb-1">2. AI Understanding</p>
                <p>We convert your query to a vector embedding using your AI key</p>
              </div>
              <div>
                <p className="font-semibold text-foreground mb-1">3. Smart Matching</p>
                <p>Find code based on semantic meaning, not just keywords</p>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  )
}
