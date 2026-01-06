"use client"

import { Alert } from '@aeptus/ui'
import { AlertCircle, CheckCircle2, Github, GitlabIcon as GitLab } from "lucide-react"
import { useState } from "react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"

interface OAuthProviderCardProps {
  provider: "github" | "gitlab" | "bitbucket"
  isConnected: boolean
  username?: string
  onConnect: () => void
  onDisconnect: () => void
  isLoading?: boolean
}

const PROVIDER_CONFIG = {
  github: {
    name: "GitHub",
    icon: Github,
    description: "Connect your GitHub account to import repositories and enable CI/CD integrations",
    color: "text-gray-900 dark:text-white"
  },
  gitlab: {
    name: "GitLab",
    icon: GitLab,
    description: "Connect your GitLab account to import repositories and enable CI/CD integrations",
    color: "text-orange-600"
  },
  bitbucket: {
    name: "Bitbucket",
    icon: Github, // Replace with Bitbucket icon when available
    description: "Connect your Bitbucket account to import repositories and enable CI/CD integrations",
    color: "text-blue-600"
  }
}

export function OAuthProviderCard({
  provider,
  isConnected,
  username,
  onConnect,
  onDisconnect,
  isLoading = false
}: OAuthProviderCardProps) {
  const [showDisconnectConfirm, setShowDisconnectConfirm] = useState(false)
  const config = PROVIDER_CONFIG[provider]
  const Icon = config.icon

  const handleDisconnect = () => {
    if (!showDisconnectConfirm) {
      setShowDisconnectConfirm(true)
      return
    }
    onDisconnect()
    setShowDisconnectConfirm(false)
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-start justify-between">
          <div className="flex items-center gap-3">
            <Icon className={`h-8 w-8 ${config.color}`} />
            <div>
              <CardTitle className="flex items-center gap-2">
                {config.name}
                {isConnected && (
                  <Badge variant="outline" className="gap-1">
                    <CheckCircle2 className="h-3 w-3 text-green-600" />
                    Connected
                  </Badge>
                )}
              </CardTitle>
              {isConnected && username && (
                <CardDescription className="mt-1">
                  Connected as <span className="font-medium">{username}</span>
                </CardDescription>
              )}
            </div>
          </div>
        </div>
        {!isConnected && (
          <CardDescription>{config.description}</CardDescription>
        )}
      </CardHeader>

      <CardFooter className="flex flex-col gap-3">
        {showDisconnectConfirm && (
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              Disconnecting will remove all synced repositories. Are you sure?
            </AlertDescription>
          </Alert>
        )}

        <div className="flex gap-2 w-full">
          {isConnected ? (
            <>
              <Button
                variant={showDisconnectConfirm ? "destructive" : "outline"}
                onClick={handleDisconnect}
                disabled={isLoading}
                className="flex-1"
              >
                {showDisconnectConfirm ? "Confirm Disconnect" : "Disconnect"}
              </Button>
              {showDisconnectConfirm && (
                <Button
                  variant="ghost"
                  onClick={() => setShowDisconnectConfirm(false)}
                  disabled={isLoading}
                >
                  Cancel
                </Button>
              )}
            </>
          ) : (
            <Button
              onClick={onConnect}
              disabled={isLoading}
              className="flex-1"
            >
              <Icon className="mr-2 h-4 w-4" />
              Connect {config.name}
            </Button>
          )}
        </div>
      </CardFooter>
    </Card>
  )
}
