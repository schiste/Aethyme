'use client'

import { ExternalLink, FileCode, Sparkles } from 'lucide-react'
import Link from 'next/link'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

interface SearchResult {
  symbol_name: string
  symbol_kind: string
  language: string
  file_path: string
  signature?: string
  documentation?: string
  code_snippet?: string
  similarity: number
  repository_id?: string
}

interface SemanticSearchResultsProps {
  results: {
    query: string
    results: SearchResult[]
    total: number
    model_used: string
    provider_used: string
  }
  query: string
}

export function SemanticSearchResults({ results, query: _query }: SemanticSearchResultsProps) {
  const getLanguageColor = (language: string) => {
    const colors: Record<string, string> = {
      python: 'bg-blue-500/10 text-blue-700 dark:text-blue-400',
      javascript: 'bg-yellow-500/10 text-yellow-700 dark:text-yellow-400',
      typescript: 'bg-blue-500/10 text-blue-700 dark:text-blue-400',
      go: 'bg-cyan-500/10 text-cyan-700 dark:text-cyan-400',
      rust: 'bg-orange-500/10 text-orange-700 dark:text-orange-400',
      java: 'bg-red-500/10 text-red-700 dark:text-red-400',
      c: 'bg-gray-500/10 text-gray-700 dark:text-gray-400',
    }
    return colors[language.toLowerCase()] || 'bg-gray-500/10 text-gray-700'
  }

  const getSymbolIcon = (kind: string) => {
    switch (kind) {
      case 'function':
        return 'ƒ'
      case 'class':
        return 'C'
      case 'method':
        return 'M'
      case 'variable':
        return 'V'
      default:
        return '•'
    }
  }

  const getSimilarityColor = (similarity: number) => {
    if (similarity >= 0.8) return 'text-green-600 dark:text-green-400'
    if (similarity >= 0.6) return 'text-yellow-600 dark:text-yellow-400'
    return 'text-orange-600 dark:text-orange-400'
  }

  if (results.total === 0) {
    return (
      <Card>
        <CardContent className="py-12 text-center">
          <Sparkles className="h-12 w-12 text-muted-foreground mx-auto mb-4" />
          <h3 className="text-lg font-semibold mb-2">No Results Found</h3>
          <p className="text-sm text-muted-foreground mb-4">
            Try adjusting your query or filters
          </p>
          <p className="text-xs text-muted-foreground">
            Make sure you have generated embeddings for your repositories
          </p>
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Search Results</h2>
          <p className="text-sm text-muted-foreground">
            Found {results.total} {results.total === 1 ? 'result' : 'results'} using{' '}
            {results.provider_used} ({results.model_used})
          </p>
        </div>
      </div>

      <div className="space-y-3">
        {results.results.map((result, index) => (
          <Card key={index} className="hover:bg-accent/50 transition-colors">
            <CardHeader className="pb-3">
              <div className="flex items-start justify-between gap-4">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 mb-2">
                    <div className="flex items-center justify-center w-6 h-6 rounded bg-primary/10 text-primary text-xs font-mono">
                      {getSymbolIcon(result.symbol_kind)}
                    </div>
                    <CardTitle className="text-lg truncate">{result.symbol_name}</CardTitle>
                    <Badge variant="secondary" className={getLanguageColor(result.language)}>
                      {result.language}
                    </Badge>
                  </div>
                  <CardDescription className="flex items-center gap-2 truncate">
                    <FileCode className="h-3 w-3 flex-shrink-0" />
                    <span className="truncate">{result.file_path}</span>
                  </CardDescription>
                </div>
                <div className="flex flex-col items-end gap-2">
                  <Badge className={getSimilarityColor(result.similarity)} variant="outline">
                    {(result.similarity * 100).toFixed(0)}% match
                  </Badge>
                  {result.repository_id && (
                    <Button variant="ghost" size="sm" asChild>
                      <Link href={`/repositories/${result.repository_id}?file=${result.file_path}`}>
                        <ExternalLink className="h-3 w-3 mr-1" />
                        View
                      </Link>
                    </Button>
                  )}
                </div>
              </div>
            </CardHeader>
            <CardContent className="space-y-2">
              {result.signature && (
                <div className="rounded bg-muted p-2">
                  <code className="text-xs font-mono">{result.signature}</code>
                </div>
              )}
              {result.documentation && (
                <p className="text-sm text-muted-foreground line-clamp-2">
                  {result.documentation}
                </p>
              )}
              {result.code_snippet && (
                <details className="text-xs">
                  <summary className="cursor-pointer text-muted-foreground hover:text-foreground">
                    Show code snippet
                  </summary>
                  <pre className="mt-2 rounded bg-muted p-2 overflow-x-auto">
                    <code className="font-mono">{result.code_snippet}</code>
                  </pre>
                </details>
              )}
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  )
}
