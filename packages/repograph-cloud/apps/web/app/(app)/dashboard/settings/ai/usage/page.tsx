'use client'

import { RefreshCw } from 'lucide-react'

import { CostEstimator } from '@/components/ai/CostEstimator'
import { ProviderBreakdown } from '@/components/ai/ProviderBreakdown'
import { TokenUsageChart } from '@/components/ai/TokenUsageChart'
import { UsageStats } from '@/components/ai/UsageStats'
import { Button } from '@/components/ui/button'
import { useAIUsage } from '@/lib/hooks/useAIUsage'

export default function AIUsagePage() {
  const { usage, loading, error, refresh } = useAIUsage()

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (error) {
    return (
      <div className="text-center py-12">
        <p className="text-destructive mb-4">{error}</p>
        <Button onClick={refresh}>Retry</Button>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">AI Usage</h1>
          <p className="text-muted-foreground mt-2">
            Monitor your AI API usage and estimated costs
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={refresh}>
          <RefreshCw className="h-4 w-4 mr-2" />
          Refresh
        </Button>
      </div>

      <UsageStats usage={usage} />

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <TokenUsageChart data={usage?.daily_usage || []} />
        <ProviderBreakdown data={usage?.by_provider || {}} />
      </div>

      <CostEstimator usage={usage} />
    </div>
  )
}
