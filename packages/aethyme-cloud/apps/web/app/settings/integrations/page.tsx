"use client"

import { AlertCircle, Loader2 } from "lucide-react"

import { Alert, AlertDescription } from "@/components/ui/alert"
import { OAuthProviderCard } from "@/components/oauth/OAuthProviderCard"
import { useOAuth } from "@/lib/hooks/use-oauth"

export default function IntegrationsPage() {
  const { connections, isLoading, error, connect, disconnect } = useOAuth()

  if (isLoading) {
    return (
      <div className="container mx-auto py-8 flex items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  return (
    <div className="container mx-auto py-8 space-y-8">
      <div>
        <h1 className="text-3xl font-bold">Integrations</h1>
        <p className="text-muted-foreground mt-2">
          Connect your code hosting platforms to import repositories and enable advanced features
        </p>
      </div>

      {error && (
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      <div className="grid gap-6 md:grid-cols-2 lg:grid-cols-3">
        {/* GitHub */}
        <OAuthProviderCard
          provider="github"
          isConnected={connections.github.isConnected}
          username={connections.github.username || undefined}
          onConnect={() => connect("github")}
          onDisconnect={() => disconnect("github")}
          isLoading={isLoading}
        />

        {/* GitLab */}
        <OAuthProviderCard
          provider="gitlab"
          isConnected={connections.gitlab.isConnected}
          username={connections.gitlab.username || undefined}
          onConnect={() => connect("gitlab")}
          onDisconnect={() => disconnect("gitlab")}
          isLoading={isLoading}
        />

        {/* Bitbucket */}
        <OAuthProviderCard
          provider="bitbucket"
          isConnected={connections.bitbucket.isConnected}
          username={connections.bitbucket.username || undefined}
          onConnect={() => connect("bitbucket")}
          onDisconnect={() => disconnect("bitbucket")}
          isLoading={isLoading}
        />
      </div>

      {/* Repository Import Section - Show if any provider is connected */}
      {(connections.github.isConnected || connections.gitlab.isConnected || connections.bitbucket.isConnected) && (
        <div className="mt-8">
          <h2 className="text-2xl font-bold mb-4">Import Repositories</h2>
          <p className="text-muted-foreground mb-4">
            Browse and import repositories from your connected platforms
          </p>
          {/* TODO: Add repository selector component */}
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              Repository import coming soon. For now, use the Repositories page to add repositories manually.
            </AlertDescription>
          </Alert>
        </div>
      )}
    </div>
  )
}
