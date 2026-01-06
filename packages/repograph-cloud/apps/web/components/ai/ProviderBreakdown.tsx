'use client'

import { Cell, Pie, PieChart, ResponsiveContainer, Tooltip } from 'recharts'

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

interface ProviderBreakdownProps {
  data: Record<
    string,
    {
      tokens: number
      requests: number
      cost: number
    }
  >
}

const COLORS = {
  openai: '#10b981',
  claude: '#8b5cf6',
  azure_openai: '#3b82f6',
}

export function ProviderBreakdown({ data }: ProviderBreakdownProps) {
  const chartData = Object.entries(data || {}).map(([provider, stats]) => ({
    name: provider === 'openai' ? 'OpenAI' : provider === 'claude' ? 'Claude' : 'Azure OpenAI',
    value: stats.tokens,
    requests: stats.requests,
    cost: stats.cost,
  }))

  const _totalTokens = chartData.reduce((sum, item) => sum + item.value, 0)

  return (
    <Card>
      <CardHeader>
        <CardTitle>Provider Breakdown</CardTitle>
        <CardDescription>Token distribution across AI providers</CardDescription>
      </CardHeader>
      <CardContent>
        {chartData.length === 0 ? (
          <div className="h-[300px] flex items-center justify-center text-sm text-muted-foreground">
            No provider data available yet
          </div>
        ) : (
          <div>
            <ResponsiveContainer width="100%" height={200}>
              <PieChart>
                <Pie
                  data={chartData}
                  cx="50%"
                  cy="50%"
                  labelLine={false}
                  label={({ name, percent }) => `${name} ${(percent * 100).toFixed(0)}%`}
                  outerRadius={80}
                  fill="#8884d8"
                  dataKey="value"
                >
                  {chartData.map((entry, index) => (
                    <Cell
                      key={`cell-${index}`}
                      fill={Object.values(COLORS)[index % Object.values(COLORS).length]}
                    />
                  ))}
                </Pie>
                <Tooltip
                  contentStyle={{
                    backgroundColor: 'hsl(var(--background))',
                    border: '1px solid hsl(var(--border))',
                    borderRadius: '6px',
                  }}
                />
              </PieChart>
            </ResponsiveContainer>

            <div className="mt-4 space-y-2">
              {chartData.map((provider, index) => (
                <div key={provider.name} className="flex items-center justify-between text-sm">
                  <div className="flex items-center gap-2">
                    <div
                      className="w-3 h-3 rounded-full"
                      style={{
                        backgroundColor: Object.values(COLORS)[index % Object.values(COLORS).length],
                      }}
                    />
                    <span className="font-medium">{provider.name}</span>
                  </div>
                  <div className="text-right">
                    <div>{provider.value.toLocaleString()} tokens</div>
                    <div className="text-xs text-muted-foreground">
                      {provider.requests} requests • ${provider.cost.toFixed(2)}
                    </div>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
