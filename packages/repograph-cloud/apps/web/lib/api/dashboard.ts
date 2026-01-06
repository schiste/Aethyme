/**
 * Dashboard statistics API client
 */

import apiClient from '../api-client'

export interface LanguageStats {
  language: string
  file_count: number
  line_count: number
  percentage: number
}

export interface DashboardStats {
  total_repositories: number
  indexed_repositories: number
  indexing_repositories: number
  failed_repositories: number
  total_files: number
  total_lines: number
  languages: LanguageStats[]
}

/**
 * Get dashboard statistics for the current organization
 */
export async function getDashboardStats(): Promise<DashboardStats> {
  const response = await apiClient.get<DashboardStats>('/dashboard/stats')
  return response.data
}
