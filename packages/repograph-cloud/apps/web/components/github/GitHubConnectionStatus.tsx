"use client"

import { Avatar } from '@aeptus/ui'
import { Calendar, Loader2, Mail, RefreshCw, User } from "lucide-react"
import { useState } from "react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Separator } from "@/components/ui/separator"
import { useGitHubConnection } from "@/lib/hooks/use-github-connection"

import { DisconnectGitHubDialog } from "./DisconnectGitHubDialog"
import { GitHubConnectButton } from "./GitHubConnectButton"

export function GitHubConnectionStatus() {
  const { isConnected, account, isLoading, error, refetch } = useGitHubConnection()
  const [isRefreshing, setIsRefreshing] = useState(false)

  const handleRefresh = async () => {
    setIsRefreshing(true)
    await refetch()
    setIsRefreshing(false)
  }

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>GitHub Connection</CardTitle>
          <CardDescription>
            Manage your GitHub account integration
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
          </div>
        </CardContent>
      </Card>
    )
  }

  if (error) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>GitHub Connection</CardTitle>
          <CardDescription>
            Manage your GitHub account integration
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="rounded-lg border border-destructive bg-destructive/10 p-4">
            <p className="text-sm text-destructive">
              Failed to load connection status: {error.message}
            </p>
            <Button
              variant="outline"
              size="sm"
              className="mt-2"
              onClick={handleRefresh}
            >
              <RefreshCw className="mr-2 h-4 w-4" />
              Retry
            </Button>
          </div>
        </CardContent>
      </Card>
    )
  }

  if (!isConnected || !account) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>GitHub Connection</CardTitle>
          <CardDescription>
            Connect your GitHub account to import and sync repositories
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="rounded-lg border border-dashed p-8">
            <div className="flex flex-col items-center justify-center space-y-3 text-center">
              <div className="rounded-full bg-muted p-3">
                <svg
                  className="h-6 w-6"
                  fill="currentColor"
                  viewBox="0 0 24 24"
                  aria-hidden="true"
                >
                  <path
                    fillRule="evenodd"
                    d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                    clipRule="evenodd"
                  />
                </svg>
              </div>
              <div>
                <h3 className="font-semibold">No GitHub Account Connected</h3>
                <p className="text-sm text-muted-foreground">
                  Connect your GitHub account to start importing repositories
                </p>
              </div>
              <GitHubConnectButton />
            </div>
          </div>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-start justify-between">
          <div>
            <CardTitle>GitHub Connection</CardTitle>
            <CardDescription>
              Your GitHub account is connected
            </CardDescription>
          </div>
          <Badge variant="default" className="bg-green-500">
            Connected
          </Badge>
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-start justify-between">
          <div className="flex items-center space-x-4">
            <Avatar className="h-16 w-16">
              <AvatarImage src={account.avatar_url || undefined} />
              <AvatarFallback>
                {account.github_username.substring(0, 2).toUpperCase()}
              </AvatarFallback>
            </Avatar>
            <div className="space-y-1">
              <div className="flex items-center space-x-2">
                <User className="h-4 w-4 text-muted-foreground" />
                <p className="font-semibold">{account.github_username}</p>
              </div>
              {account.github_email && (
                <div className="flex items-center space-x-2">
                  <Mail className="h-4 w-4 text-muted-foreground" />
                  <p className="text-sm text-muted-foreground">
                    {account.github_email}
                  </p>
                </div>
              )}
              <div className="flex items-center space-x-2">
                <Calendar className="h-4 w-4 text-muted-foreground" />
                <p className="text-sm text-muted-foreground">
                  Connected {new Date(account.created_at).toLocaleDateString()}
                </p>
              </div>
            </div>
          </div>
        </div>

        {account.scopes && (
          <>
            <Separator />
            <div className="space-y-2">
              <p className="text-sm font-medium">Permissions</p>
              <div className="flex flex-wrap gap-2">
                {account.scopes.split(" ").map((scope) => (
                  <Badge key={scope} variant="secondary">
                    {scope}
                  </Badge>
                ))}
              </div>
            </div>
          </>
        )}

        <Separator />

        <div className="flex space-x-2">
          <Button
            variant="outline"
            size="sm"
            onClick={handleRefresh}
            disabled={isRefreshing}
          >
            {isRefreshing ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : (
              <RefreshCw className="mr-2 h-4 w-4" />
            )}
            Refresh
          </Button>
          <DisconnectGitHubDialog />
        </div>
      </CardContent>
    </Card>
  )
}
