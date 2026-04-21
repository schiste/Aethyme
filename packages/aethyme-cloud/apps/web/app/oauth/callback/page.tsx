"use client"

import { CheckCircle2, Loader2,XCircle } from "lucide-react"
import { useRouter, useSearchParams } from "next/navigation"
import { useEffect, useState } from "react"

import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"

export default function OAuthCallbackPage() {
  const router = useRouter()
  const searchParams = useSearchParams()
  const [status, setStatus] = useState<"loading" | "success" | "error">("loading")
  const [error, setError] = useState<string | null>(null)
  const [provider, setProvider] = useState<string>("")

  useEffect(() => {
    const handleCallback = async () => {
      const code = searchParams.get("code")
      const state = searchParams.get("state")
      const providerParam = searchParams.get("provider")
      const errorParam = searchParams.get("error")

      if (errorParam) {
        setStatus("error")
        setError(`OAuth error: ${errorParam}`)
        return
      }

      if (!code || !state || !providerParam) {
        setStatus("error")
        setError("Missing required parameters")
        return
      }

      setProvider(providerParam)

      try {
        // Get access token from localStorage (set during authorization flow)
        const token = localStorage.getItem("access_token")

        if (!token) {
          throw new Error("Not authenticated. Please log in first.")
        }

        // Exchange code for access token
        const response = await fetch(`${process.env.NEXT_PUBLIC_API_URL}/api/v1/oauth/callback`, {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            "Authorization": `Bearer ${token}`
          },
          body: JSON.stringify({
            code,
            state,
            provider: providerParam
          })
        })

        if (!response.ok) {
          const errorData = await response.json()
          throw new Error(errorData.detail || "Failed to connect account")
        }

        const _data = await response.json()

        setStatus("success")

        // Redirect to integrations page after 2 seconds
        setTimeout(() => {
          router.push("/settings/integrations")
        }, 2000)

      } catch (err) {
        setStatus("error")
        setError(err instanceof Error ? err.message : "An unexpected error occurred")
      }
    }

    handleCallback()
  }, [searchParams, router])

  const getProviderName = (provider: string) => {
    const names: Record<string, string> = {
      github: "GitHub",
      gitlab: "GitLab",
      bitbucket: "Bitbucket"
    }
    return names[provider] || provider
  }

  return (
    <div className="container flex items-center justify-center min-h-screen py-8">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            {status === "loading" && (
              <>
                <Loader2 className="h-5 w-5 animate-spin" />
                Connecting {provider && getProviderName(provider)}...
              </>
            )}
            {status === "success" && (
              <>
                <CheckCircle2 className="h-5 w-5 text-green-600" />
                Successfully Connected!
              </>
            )}
            {status === "error" && (
              <>
                <XCircle className="h-5 w-5 text-red-600" />
                Connection Failed
              </>
            )}
          </CardTitle>
          <CardDescription>
            {status === "loading" && "Please wait while we connect your account..."}
            {status === "success" && `Your ${provider && getProviderName(provider)} account has been connected successfully.`}
            {status === "error" && "We encountered an error while connecting your account."}
          </CardDescription>
        </CardHeader>

        <CardContent className="space-y-4">
          {status === "success" && (
            <Alert className="border-green-200 bg-green-50 dark:bg-green-950/20">
              <CheckCircle2 className="h-4 w-4 text-green-600" />
              <AlertDescription className="text-green-900 dark:text-green-100">
                Redirecting you to integrations page...
              </AlertDescription>
            </Alert>
          )}

          {status === "error" && (
            <>
              <Alert variant="destructive">
                <XCircle className="h-4 w-4" />
                <AlertDescription>{error}</AlertDescription>
              </Alert>
              <Button
                onClick={() => router.push("/settings/integrations")}
                className="w-full"
              >
                Back to Integrations
              </Button>
            </>
          )}

          {status === "loading" && (
            <div className="flex justify-center py-8">
              <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
