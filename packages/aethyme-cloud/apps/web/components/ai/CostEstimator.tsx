'use client'

import { Calculator } from 'lucide-react'
import { useState } from 'react'

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'

interface CostEstimatorProps {
  usage: any
}

const PRICING = {
  openai: {
    'text-embedding-3-small': { input: 0.02 / 1_000_000 },
    'text-embedding-3-large': { input: 0.13 / 1_000_000 },
    'gpt-4-turbo': { input: 0.01 / 1000, output: 0.03 / 1000 },
    'gpt-4': { input: 0.03 / 1000, output: 0.06 / 1000 },
  },
  claude: {
    'claude-3-5-sonnet': { input: 0.003 / 1000, output: 0.015 / 1000 },
    'claude-3-opus': { input: 0.015 / 1000, output: 0.075 / 1000 },
  },
}

export function CostEstimator({ usage: _usage }: CostEstimatorProps) {
  const [symbolCount, setSymbolCount] = useState(50000)
  const [queryCount, setQueryCount] = useState(1000)
  const [provider, setProvider] = useState('openai')
  const [embeddingModel, setEmbeddingModel] = useState('text-embedding-3-small')

  const calculateCosts = () => {
    // Estimate: ~100 tokens per symbol for embeddings
    const embeddingTokens = symbolCount * 100
    const embeddingCost = embeddingTokens * 0.02 / 1_000_000

    // Estimate: ~20 tokens per search query
    const searchTokens = queryCount * 20
    const searchCost = searchTokens * 0.02 / 1_000_000

    return {
      oneTime: embeddingCost,
      monthly: searchCost,
      total: embeddingCost + searchCost,
    }
  }

  const costs = calculateCosts()

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-2">
          <Calculator className="h-5 w-5" />
          <CardTitle>Cost Estimator</CardTitle>
        </div>
        <CardDescription>
          Estimate your AI costs based on codebase size and usage
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label htmlFor="symbolCount">Number of Symbols (functions, classes)</Label>
            <Input
              id="symbolCount"
              type="number"
              value={symbolCount}
              onChange={(e) => setSymbolCount(parseInt(e.target.value) || 0)}
              min={0}
              step={1000}
            />
            <p className="text-xs text-muted-foreground">
              Typical: Small repo ~10K, Medium ~50K, Large ~200K
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="queryCount">Monthly Semantic Searches</Label>
            <Input
              id="queryCount"
              type="number"
              value={queryCount}
              onChange={(e) => setQueryCount(parseInt(e.target.value) || 0)}
              min={0}
              step={100}
            />
            <p className="text-xs text-muted-foreground">
              Estimated searches per month
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="provider">Provider</Label>
            <Select value={provider} onValueChange={setProvider}>
              <SelectTrigger id="provider">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="openai">OpenAI</SelectItem>
                <SelectItem value="claude">Claude (embeddings via OpenAI)</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-2">
            <Label htmlFor="model">Embedding Model</Label>
            <Select value={embeddingModel} onValueChange={setEmbeddingModel}>
              <SelectTrigger id="model">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="text-embedding-3-small">
                  text-embedding-3-small ($0.02/1M tokens)
                </SelectItem>
                <SelectItem value="text-embedding-3-large">
                  text-embedding-3-large ($0.13/1M tokens)
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        <Separator />

        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <div>
              <div className="font-medium">Initial Embedding Generation</div>
              <div className="text-sm text-muted-foreground">
                One-time cost for {symbolCount.toLocaleString()} symbols
              </div>
            </div>
            <div className="text-2xl font-bold text-green-600 dark:text-green-400">
              ${costs.oneTime.toFixed(2)}
            </div>
          </div>

          <div className="flex items-center justify-between">
            <div>
              <div className="font-medium">Monthly Search Cost</div>
              <div className="text-sm text-muted-foreground">
                {queryCount.toLocaleString()} searches/month
              </div>
            </div>
            <div className="text-2xl font-bold text-blue-600 dark:text-blue-400">
              ${costs.monthly.toFixed(2)}/mo
            </div>
          </div>

          <Separator />

          <div className="flex items-center justify-between">
            <div>
              <div className="font-semibold text-lg">Estimated Total (First Month)</div>
              <div className="text-sm text-muted-foreground">
                Includes initial setup + first month of searches
              </div>
            </div>
            <div className="text-3xl font-bold text-primary">
              ${costs.total.toFixed(2)}
            </div>
          </div>
        </div>

        <div className="rounded-lg bg-muted p-4">
          <p className="text-sm text-muted-foreground">
            <strong>Note:</strong> These are rough estimates. Actual costs depend on code complexity,
            documentation length, and query patterns. Incremental updates after initial indexing are
            much cheaper (~10% of full cost).
          </p>
        </div>
      </CardContent>
    </Card>
  )
}
