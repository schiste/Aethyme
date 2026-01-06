import type {
  CancelJobResponse,
  IndexRepositoryRequest,
  IndexRepositoryResponse,
  JobStatusResponse,
} from '../../types/jobs'
import apiClient from '../api-client'

export const jobsApi = {
  /**
   * Get job status by job ID
   */
  async getJobStatus(jobId: string): Promise<JobStatusResponse> {
    const { data } = await apiClient.get<JobStatusResponse>(`/jobs/${jobId}`)
    return data
  },

  /**
   * Trigger repository indexing
   */
  async indexRepository(
    request: IndexRepositoryRequest
  ): Promise<IndexRepositoryResponse> {
    const { data } = await apiClient.post<IndexRepositoryResponse>(
      '/jobs/index-repository',
      request
    )
    return data
  },

  /**
   * Cancel a running job
   */
  async cancelJob(jobId: string): Promise<CancelJobResponse> {
    const { data } = await apiClient.delete<CancelJobResponse>(
      `/jobs/${jobId}`
    )
    return data
  },

  /**
   * List all jobs (optional - for future use)
   */
  async listJobs(): Promise<JobStatusResponse[]> {
    const { data } = await apiClient.get<JobStatusResponse[]>('/jobs')
    return data
  },
}
