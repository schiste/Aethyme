'use client'


import { StatsCards } from '@/components/dashboard/StatsCards'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

export default function DashboardPage() {
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-3xl font-bold tracking-tight">Welcome back!</h1>
        <p className="text-muted-foreground mt-2">
          Here's an overview of your RepoGraph Cloud account
        </p>
      </div>

      {/* Live Statistics */}
      <StatsCards />

      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Getting Started</CardTitle>
            <CardDescription>
              Quick steps to get up and running
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-2">
            <div className="flex items-start gap-2">
              <div className="rounded-full bg-primary/10 p-1 mt-0.5">
                <div className="h-2 w-2 rounded-full bg-primary" />
              </div>
              <div>
                <p className="text-sm font-medium">Connect a repository</p>
                <p className="text-xs text-muted-foreground">
                  Add your first code repository to start indexing
                </p>
              </div>
            </div>
            <div className="flex items-start gap-2">
              <div className="rounded-full bg-slate-200 dark:bg-slate-800 p-1 mt-0.5">
                <div className="h-2 w-2 rounded-full bg-slate-400 dark:bg-slate-600" />
              </div>
              <div>
                <p className="text-sm font-medium">Generate an API key</p>
                <p className="text-xs text-muted-foreground">
                  Create an API key for programmatic access
                </p>
              </div>
            </div>
            <div className="flex items-start gap-2">
              <div className="rounded-full bg-slate-200 dark:bg-slate-800 p-1 mt-0.5">
                <div className="h-2 w-2 rounded-full bg-slate-400 dark:bg-slate-600" />
              </div>
              <div>
                <p className="text-sm font-medium">Start querying</p>
                <p className="text-xs text-muted-foreground">
                  Use the API to query your code knowledge graph
                </p>
              </div>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Recent Activity</CardTitle>
            <CardDescription>
              Your latest actions and events
            </CardDescription>
          </CardHeader>
          <CardContent>
            <p className="text-sm text-muted-foreground text-center py-8">
              No activity yet
            </p>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
