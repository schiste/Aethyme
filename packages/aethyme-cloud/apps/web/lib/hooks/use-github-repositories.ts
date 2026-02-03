import { useCallback,useEffect, useState } from 'react'

import type { GitHubRepository } from '../../types/github'
import { githubApi } from '../api/github'

export function useGitHubRepositories() {
  const [repositories, setRepositories] = useState<GitHubRepository[]>([])
  const [isLoading, setIsLoading] = useState(true)
  const [error, setError] = useState<Error | null>(null)

  const fetchRepositories = useCallback(async () => {
    try {
      setIsLoading(true)
      setError(null)
      const { repositories: repos } = await githubApi.getRepositories()
      setRepositories(repos)
    } catch (err) {
      setError(err instanceof Error ? err : new Error('Failed to fetch repositories'))
      setRepositories([])
    } finally {
      setIsLoading(false)
    }
  }, [])

  useEffect(() => {
    fetchRepositories()
  }, [fetchRepositories])

  return {
    repositories,
    isLoading,
    error,
    refetch: fetchRepositories,
  }
}
