'use client'

import { Lightbulb } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'

interface ExampleQueriesProps {
  onExampleClick: (query: string) => void
}

const EXAMPLES = [
  {
    category: 'Functions',
    queries: [
      'function that validates email addresses',
      'async function for fetching user data',
      'error handling utility function',
    ],
  },
  {
    category: 'Classes',
    queries: [
      'class for managing database connections',
      'user authentication service class',
      'API client implementation',
    ],
  },
  {
    category: 'APIs',
    queries: [
      'REST endpoint for user registration',
      'GraphQL mutation for updating profiles',
      'webhook handler for payment events',
    ],
  },
  {
    category: 'Utilities',
    queries: [
      'date formatting helper',
      'string manipulation utilities',
      'logging configuration',
    ],
  },
]

export function ExampleQueries({ onExampleClick }: ExampleQueriesProps) {
  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-2">
          <Lightbulb className="h-4 w-4 text-yellow-500" />
          <CardTitle className="text-sm">Example Queries</CardTitle>
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        {EXAMPLES.map((category) => (
          <div key={category.category}>
            <h4 className="text-xs font-semibold text-muted-foreground mb-2">
              {category.category}
            </h4>
            <div className="space-y-1">
              {category.queries.map((query) => (
                <Button
                  key={query}
                  variant="ghost"
                  size="sm"
                  className="w-full justify-start text-xs h-auto py-2 px-2 whitespace-normal text-left"
                  onClick={() => onExampleClick(query)}
                >
                  {query}
                </Button>
              ))}
            </div>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}
