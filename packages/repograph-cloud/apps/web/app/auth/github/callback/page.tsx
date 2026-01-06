"use client"

import { CheckCircle, Loader2, XCircle } from "lucide-react"
import { useRouter, useSearchParams } from "next/navigation"
import { useEffect, useState } from "react"

import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import apiClient from "@/lib/api-client"

export default function GitHubCallbackPage() {
  const router = useRouter()
  const searchParams = useSearchParams()
  const [status, setStatus] = useState<"loading" | "success" | "error">("loading")
  const [message, setMessage] = useState("Processing GitHub authentication...")
  const [account, setAccount] = useState<any>(null)

  useEffect(() => {
    const processCallback = async () => {
      const code = searchParams.get("code")
      const state = searchParams.get("state")
      const error = searchParams.get("error")

      // Handle OAuth error
      if (error) {
        setStatus("error")
        setMessage(`GitHub OAuth error: ${error}`)
        return
      }

      // Validate parameters
      if (!code || !state) {
        setStatus("error")
        setMessage("Missing required OAuth parameters")
        return
      }

      try {
        // Call backend callback endpoint
        const { data } = await apiClient.get(
          `/auth/github/callback?code=${code}&state=${state}`
        )

        setAccount(data)
        setStatus("success")
        setMessage("Successfully connected your GitHub account!")

        // Redirect to settings after 2 seconds
        setTimeout(() => {
          router.push("/settings/integrations")
        }, 2000)
      } catch (err: any) {
        setStatus("error")
        const errorMessage = err.response?.data?.detail || err.message || "Failed to connect GitHub account"
        setMessage(errorMessage)
      }
    }

    processCallback()
  }, [searchParams, router])

  return (
    <div className="flex min-h-screen items-center justify-center p-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle>GitHub OAuth</CardTitle>
          <CardDescription>
            {status === "loading" && "Processing your GitHub connection..."}
            {status === "success" && "Connection successful!"}
            {status === "error" && "Connection failed"}
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex flex-col items-center justify-center space-y-4">
            {status === "loading" && (
              <Loader2 className="h-12 w-12 animate-spin text-primary" />
            )}
            {status === "success" && (
              <CheckCircle className="h-12 w-12 text-green-500" />
            )}
            {status === "error" && (
              <XCircle className="h-12 w-12 text-destructive" />
            )}

            <p className="text-center text-sm text-muted-foreground">
              {message}
            </p>

            {account && (
              <div className="w-full rounded-lg border p-4">
                <div className="flex items-center space-x-3">
                  {account.avatar_url && (
                    <img
                      src={account.avatar_url}
                      alt={account.github_username}
                      className="h-10 w-10 rounded-full"
                    />
                  )}
                  <div>
                    <p className="font-medium">{account.github_username}</p>
                    {account.github_email && (
                      <p className="text-sm text-muted-foreground">
                        {account.github_email}
                      </p>
                    )}
                  </div>
                </div>
              </div>
            )}

            {status === "success" && (
              <p className="text-xs text-muted-foreground">
                Redirecting to settings...
              </p>
            )}

            {status === "error" && (
              <div className="flex space-x-2">
                <Button
                  variant="outline"
                  onClick={() => router.push("/dashboard")}
                >
                  Go to Dashboard
                </Button>
                <Button onClick={() => router.push("/settings/integrations")}>
                  Try Again
                </Button>
              </div>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
