import { useCallback,useEffect, useState } from 'react'

import type { GitHubAccount, GitHubConnectionStatus } from '../../types/github'
import { githubApi } from '../api/github'

export function useGitHubConnection() {
  const [isConnected, setIsConnected] = useState(false)
  const [account, setAccount] = useState<GitHubAccount | null>(null)
  const [isLoading, setIsLoading] = useState(true)
  const [error, setError] = useState<Error | null>(null)

  const fetchStatus = useCallback(async () => {
    try {
      setIsLoading(true)
      setError(null)
      const status: GitHubConnectionStatus = await githubApi.getConnectionStatus()
      setIsConnected(status.connected)
      setAccount(status.account || null)
    } catch (err) {
      setError(err instanceof Error ? err : new Error('Failed to fetch connection status'))
      setIsConnected(false)
      setAccount(null)
    } finally {
      setIsLoading(false)
    }
  }, [])

  useEffect(() => {
    fetchStatus()
  }, [fetchStatus])

  const connect = useCallback(async () => {
    try {
      setError(null)
      const { authorization_url } = await githubApi.getAuthorizationUrl()
      // Redirect to GitHub OAuth
      window.location.href = authorization_url
    } catch (err) {
      setError(err instanceof Error ? err : new Error('Failed to start OAuth flow'))
    }
  }, [])

  const disconnect = useCallback(async () => {
    try {
      setError(null)
      await githubApi.disconnect()
      setIsConnected(false)
      setAccount(null)
    } catch (err) {
      setError(err instanceof Error ? err : new Error('Failed to disconnect'))
      throw err
    }
  }, [])

  return {
    isConnected,
    account,
    isLoading,
    error,
    connect,
    disconnect,
    refetch: fetchStatus,
  }
}
