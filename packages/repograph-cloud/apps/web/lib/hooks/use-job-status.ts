import { useCallback, useEffect, useRef,useState } from 'react'

import type {JobStatusResponse } from '../../types/jobs'
import { jobsApi } from '../api/jobs'

interface UseJobStatusOptions {
  pollInterval?: number // milliseconds
  enabled?: boolean
}

export function useJobStatus(
  jobId: string | null,
  options: UseJobStatusOptions = {}
) {
  const { pollInterval = 2000, enabled = true } = options

  const [status, setStatus] = useState<JobStatusResponse | null>(null)
  const [isLoading, setIsLoading] = useState(false)
  const [error, setError] = useState<Error | null>(null)
  const pollIntervalRef = useRef<NodeJS.Timeout | null>(null)

  const fetchStatus = useCallback(async () => {
    if (!jobId || !enabled) return

    try {
      setIsLoading(true)
      setError(null)
      const data = await jobsApi.getJobStatus(jobId)
      setStatus(data)

      // Stop polling if job is complete or failed
      if (
        data.status === 'SUCCESS' ||
        data.status === 'FAILURE' ||
        data.status === 'REVOKED'
      ) {
        if (pollIntervalRef.current) {
          clearInterval(pollIntervalRef.current)
          pollIntervalRef.current = null
        }
      }
    } catch (err) {
      setError(err instanceof Error ? err : new Error('Failed to fetch job status'))
    } finally {
      setIsLoading(false)
    }
  }, [jobId, enabled])

  useEffect(() => {
    if (!jobId || !enabled) {
      setStatus(null)
      return
    }

    // Initial fetch
    fetchStatus()

    // Start polling
    pollIntervalRef.current = setInterval(fetchStatus, pollInterval)

    return () => {
      if (pollIntervalRef.current) {
        clearInterval(pollIntervalRef.current)
        pollIntervalRef.current = null
      }
    }
  }, [jobId, enabled, fetchStatus, pollInterval])

  const progress = status?.progress ?? status?.meta?.progress ?? 0
  const isComplete = status?.status === 'SUCCESS'
  const isFailed = status?.status === 'FAILURE'
  const isPending = status?.status === 'PENDING'
  const isRunning =
    status?.status === 'INDEXING' || status?.status === 'SYNCING'

  return {
    status,
    isLoading,
    error,
    progress,
    isComplete,
    isFailed,
    isPending,
    isRunning,
    refetch: fetchStatus,
  }
}
