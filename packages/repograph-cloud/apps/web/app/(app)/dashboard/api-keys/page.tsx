import { APIKeyList } from '@/components/api-keys/api-key-list'
import { CreateAPIKeyDialog } from '@/components/api-keys/create-api-key-dialog'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

export default function APIKeysPage() {
  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">API Keys</h1>
          <p className="text-muted-foreground mt-2">
            Manage your API keys for programmatic access
          </p>
        </div>
        <CreateAPIKeyDialog />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Your API Keys</CardTitle>
          <CardDescription>
            API keys allow you to authenticate requests to the RepoGraph API
          </CardDescription>
        </CardHeader>
        <CardContent>
          <APIKeyList />
        </CardContent>
      </Card>
    </div>
  )
}
