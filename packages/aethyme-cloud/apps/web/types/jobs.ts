export type JobStatus =
  | "PENDING"
  | "INDEXING"
  | "SYNCING"
  | "SUCCESS"
  | "FAILURE"
  | "REVOKED"

export interface JobMeta {
  repository_id?: string
  progress?: number
  status?: string
  [key: string]: any
}

export interface JobStatusResponse {
  job_id: string
  status: JobStatus
  progress?: number
  result?: any
  error?: string
  meta?: JobMeta
}

export interface IndexRepositoryRequest {
  repository_id: string
}

export interface IndexRepositoryResponse {
  job_id: string
  repository_id: string
  status: string
  message: string
}

export interface CancelJobResponse {
  message: string
}
