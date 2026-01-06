import { ConnectRepositoryDialog } from '@/components/repositories/connect-repository-dialog'
import { RepositoryList } from '@/components/repositories/repository-list'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

export default function RepositoriesPage() {
  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">Repositories</h1>
          <p className="text-muted-foreground mt-2">
            Manage your connected code repositories
          </p>
        </div>
        <ConnectRepositoryDialog />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Connected Repositories</CardTitle>
          <CardDescription>
            View and manage repositories that are being indexed
          </CardDescription>
        </CardHeader>
        <CardContent>
          <RepositoryList />
        </CardContent>
      </Card>
    </div>
  )
}
