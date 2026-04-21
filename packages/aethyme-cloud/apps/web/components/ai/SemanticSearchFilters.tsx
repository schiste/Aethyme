'use client'

import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { useRepositories } from '@/lib/hooks/useRepositories'

interface FiltersState {
  repository_id: string
  language: string
  symbol_kind: string
  limit: number
}

interface SemanticSearchFiltersProps {
  filters: FiltersState
  onFiltersChange: (filters: FiltersState) => void
}

export function SemanticSearchFilters({ filters, onFiltersChange }: SemanticSearchFiltersProps) {
  const { repositories } = useRepositories()

  const updateFilter = (key: keyof FiltersState, value: string | number) => {
    onFiltersChange({ ...filters, [key]: value })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Filters</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="repository">Repository</Label>
          <Select
            value={filters.repository_id}
            onValueChange={(value) => updateFilter('repository_id', value)}
          >
            <SelectTrigger id="repository">
              <SelectValue placeholder="All repositories" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="">All repositories</SelectItem>
              {repositories?.map((repo: any) => (
                <SelectItem key={repo.id} value={repo.id}>
                  {repo.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="space-y-2">
          <Label htmlFor="language">Language</Label>
          <Select
            value={filters.language}
            onValueChange={(value) => updateFilter('language', value)}
          >
            <SelectTrigger id="language">
              <SelectValue placeholder="All languages" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="">All languages</SelectItem>
              <SelectItem value="python">Python</SelectItem>
              <SelectItem value="javascript">JavaScript</SelectItem>
              <SelectItem value="typescript">TypeScript</SelectItem>
              <SelectItem value="go">Go</SelectItem>
              <SelectItem value="rust">Rust</SelectItem>
              <SelectItem value="java">Java</SelectItem>
              <SelectItem value="c">C</SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div className="space-y-2">
          <Label htmlFor="symbolKind">Symbol Type</Label>
          <Select
            value={filters.symbol_kind}
            onValueChange={(value) => updateFilter('symbol_kind', value)}
          >
            <SelectTrigger id="symbolKind">
              <SelectValue placeholder="All types" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="">All types</SelectItem>
              <SelectItem value="function">Functions</SelectItem>
              <SelectItem value="class">Classes</SelectItem>
              <SelectItem value="method">Methods</SelectItem>
              <SelectItem value="variable">Variables</SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div className="space-y-2">
          <Label htmlFor="limit">Results Limit</Label>
          <Select
            value={filters.limit.toString()}
            onValueChange={(value) => updateFilter('limit', parseInt(value))}
          >
            <SelectTrigger id="limit">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="5">5 results</SelectItem>
              <SelectItem value="10">10 results</SelectItem>
              <SelectItem value="20">20 results</SelectItem>
              <SelectItem value="50">50 results</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </CardContent>
    </Card>
  )
}
