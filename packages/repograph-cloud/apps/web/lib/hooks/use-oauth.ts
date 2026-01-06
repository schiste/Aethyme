"use client"

import { useRouter } from "next/navigation"
import { useEffect,useState } from "react"

interface OAuthConnection {
  provider: "github" | "gitlab" | "bitbucket"
  username: string | null
  isConnected: boolean
}

interface UseOAuthResult {
  connections: Record<string, OAuthConnection>
  isLoading: boolean
  error: string | null
  connect: (provider: "github" | "gitlab" | "bitbucket") => Promise<void>
  disconnect: (provider: "github" | "gitlab" | "bitbucket") => Promise<void>
  refresh: () => Promise<void>
}

export function useOAuth(): UseOAuthResult {
  const _router = useRouter()
  const [connections, setConnections] = useState<Record<string, OAuthConnection>>({
    github: { provider: "github", username: null, isConnected: false },
    gitlab: { provider: "gitlab", username: null, isConnected: false },
    bitbucket: { provider: "bitbucket", username: null, isConnected: false }
  })
  const [isLoading, setIsLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const fetchConnectionStatus = async () => {
    try {
      const token = localStorage.getItem("access_token")
      if (!token) {
        throw new Error("Not authenticated")
      }

      const response = await fetch(`${process.env.NEXT_PUBLIC_API_URL}/api/v1/users/me`, {
        headers: {
          Authorization: `Bearer ${token}`
        }
      })

      if (!response.ok) {
        throw new Error("Failed to fetch user data")
      }

      const userData = await response.json()

      setConnections({
        github: {
          provider: "github",
          username: userData.github_username,
          isConnected: !!userData.github_id
        },
        gitlab: {
          provider: "gitlab",
          username: userData.gitlab_username,
          isConnected: !!userData.gitlab_id
        },
        bitbucket: {
          provider: "bitbucket",
          username: userData.bitbucket_username,
          isConnected: !!userData.bitbucket_id
        }
      })

      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : "An error occurred")
    } finally {
      setIsLoading(false)
    }
  }

  useEffect(() => {
    fetchConnectionStatus()
  }, [])

  const connect = async (provider: "github" | "gitlab" | "bitbucket") => {
    try {
      setIsLoading(true)
      const token = localStorage.getItem("access_token")

      if (!token) {
        throw new Error("Not authenticated")
      }

      // Start OAuth flow
      const response = await fetch(
        `${process.env.NEXT_PUBLIC_API_URL}/api/v1/oauth/${provider}/authorize`,
        {
          headers: {
            Authorization: `Bearer ${token}`
          }
        }
      )

      if (!response.ok) {
        throw new Error("Failed to start OAuth flow")
      }

      const data = await response.json()

      // Store state for validation
      sessionStorage.setItem("oauth_state", data.state)
      sessionStorage.setItem("oauth_provider", provider)

      // Redirect to OAuth provider
      window.location.href = data.authorization_url
    } catch (err) {
      setError(err instanceof Error ? err.message : "An error occurred")
      setIsLoading(false)
    }
  }

  const disconnect = async (provider: "github" | "gitlab" | "bitbucket") => {
    try {
      setIsLoading(true)
      const token = localStorage.getItem("access_token")

      if (!token) {
        throw new Error("Not authenticated")
      }

      const response = await fetch(
        `${process.env.NEXT_PUBLIC_API_URL}/api/v1/oauth/${provider}/disconnect`,
        {
          method: "DELETE",
          headers: {
            Authorization: `Bearer ${token}`
          }
        }
      )

      if (!response.ok) {
        throw new Error("Failed to disconnect")
      }

      // Update local state
      setConnections(prev => ({
        ...prev,
        [provider]: {
          provider,
          username: null,
          isConnected: false
        }
      }))

      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : "An error occurred")
    } finally {
      setIsLoading(false)
    }
  }

  const refresh = async () => {
    setIsLoading(true)
    await fetchConnectionStatus()
  }

  return {
    connections,
    isLoading,
    error,
    connect,
    disconnect,
    refresh
  }
}
