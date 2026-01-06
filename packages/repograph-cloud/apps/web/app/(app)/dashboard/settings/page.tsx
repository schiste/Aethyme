import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

export default function SettingsPage() {
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-3xl font-bold tracking-tight">Settings</h1>
        <p className="text-muted-foreground mt-2">
          Manage your account and preferences
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Account Settings</CardTitle>
          <CardDescription>
            Update your account information and preferences
          </CardDescription>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground">
            Settings page coming soon...
          </p>
        </CardContent>
      </Card>
    </div>
  )
}
