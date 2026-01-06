'use client'

import { Activity, DollarSign, TrendingUp,Zap } from 'lucide-react'

import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'

interface UsageStatsProps {
  usage: {
    total_tokens: number
    total_requests: number
    estimated_cost: number
    change_percentage: number
  } | null
}

export function UsageStats({ usage }: UsageStatsProps) {
  if (!usage) return null

  const stats = [
    {
      title: 'Total Tokens',
      value: usage.total_tokens.toLocaleString(),
      icon: Activity,
      color: 'text-blue-600 dark:text-blue-400',
    },
    {
      title: 'Total Requests',
      value: usage.total_requests.toLocaleString(),
      icon: Zap,
      color: 'text-purple-600 dark:text-purple-400',
    },
    {
      title: 'Estimated Cost',
      value: `$${usage.estimated_cost.toFixed(2)}`,
      icon: DollarSign,
      color: 'text-green-600 dark:text-green-400',
    },
    {
      title: 'Change (7d)',
      value: `${usage.change_percentage > 0 ? '+' : ''}${usage.change_percentage.toFixed(1)}%`,
      icon: TrendingUp,
      color:
        usage.change_percentage > 0
          ? 'text-red-600 dark:text-red-400'
          : 'text-green-600 dark:text-green-400',
    },
  ]

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
      {stats.map((stat) => (
        <Card key={stat.title}>
          <CardHeader className="flex flex-row items-center justify-between pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              {stat.title}
            </CardTitle>
            <stat.icon className={`h-4 w-4 ${stat.color}`} />
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{stat.value}</div>
          </CardContent>
        </Card>
      ))}
    </div>
  )
}
