export interface GitHubAccount {
  id: string
  user_id: string
  github_user_id: string
  github_username: string
  github_email: string | null
  scopes: string | null
  avatar_url: string | null
  created_at: string
  updated_at: string
}

export interface GitHubAuthorizationURL {
  authorization_url: string
  state: string
}

export interface GitHubConnectionStatus {
  connected: boolean
  account?: GitHubAccount
}

export interface GitHubRepository {
  id: number
  name: string
  full_name: string
  description: string | null
  html_url: string
  language: string | null
  stargazers_count: number
  forks_count: number
  updated_at: string
  private: boolean
  owner: {
    login: string
    avatar_url: string
  }
}

export interface GitHubRepositoriesResponse {
  repositories: GitHubRepository[]
}

export interface DisconnectResponse {
  message: string
}
