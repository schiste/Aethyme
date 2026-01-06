import type {
  DisconnectResponse,
  GitHubAuthorizationURL,
  GitHubConnectionStatus,
  GitHubRepositoriesResponse,
} from '../../types/github'
import apiClient from '../api-client'

export const githubApi = {
  /**
   * Get GitHub OAuth authorization URL
   */
  async getAuthorizationUrl(): Promise<GitHubAuthorizationURL> {
    const { data } = await apiClient.get<GitHubAuthorizationURL>(
      '/auth/github/authorize'
    )
    return data
  },

  /**
   * Get current GitHub connection status
   */
  async getConnectionStatus(): Promise<GitHubConnectionStatus> {
    const { data } = await apiClient.get<GitHubConnectionStatus>(
      '/auth/github/status'
    )
    return data
  },

  /**
   * Get user's GitHub repositories
   */
  async getRepositories(): Promise<GitHubRepositoriesResponse> {
    const { data } = await apiClient.get<GitHubRepositoriesResponse>(
      '/auth/github/repositories'
    )
    return data
  },

  /**
   * Disconnect GitHub account
   */
  async disconnect(): Promise<DisconnectResponse> {
    const { data } = await apiClient.delete<DisconnectResponse>(
      '/auth/github/disconnect'
    )
    return data
  },
}
