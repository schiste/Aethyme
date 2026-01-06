"use client"

import {
  CheckCircle,
  Clock,
  Code,
  FileText,
  Loader2,
  X,
  XCircle,
} from "lucide-react"
import { useEffect, useState } from "react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Progress } from "@/components/ui/progress"
import { jobsApi } from "@/lib/api/jobs"
import { useJobStatus } from "@/lib/hooks/use-job-status"

interface ImportProgressTrackerProps {
  jobId: string
  repositoryName: string
  onClose: () => void
}

export function ImportProgressTracker({
  jobId,
  repositoryName,
  onClose,
}: ImportProgressTrackerProps) {
  const { status, progress, isComplete, isFailed, isPending, isRunning } =
    useJobStatus(jobId)
  const [elapsedTime, setElapsedTime] = useState(0)
  const [isCancelling, setIsCancelling] = useState(false)

  // Track elapsed time
  useEffect(() => {
    if (!isRunning && !isPending) return

    const interval = setInterval(() => {
      setElapsedTime((prev) => prev + 1)
    }, 1000)

    return () => clearInterval(interval)
  }, [isRunning, isPending])

  const handleCancel = async () => {
    try {
      setIsCancelling(true)
      await jobsApi.cancelJob(jobId)
      setTimeout(onClose, 1000)
    } catch (err) {
      console.error("Failed to cancel job:", err)
    } finally {
      setIsCancelling(false)
    }
  }

  const formatTime = (seconds: number) => {
    const mins = Math.floor(seconds / 60)
    const secs = seconds % 60
    return `${mins}:${secs.toString().padStart(2, "0")}`
  }

  const getStatusBadge = () => {
    if (isPending) {
      return <Badge variant="secondary">Pending</Badge>
    }
    if (isRunning) {
      return (
        <Badge variant="default" className="bg-blue-500">
          In Progress
        </Badge>
      )
    }
    if (isComplete) {
      return (
        <Badge variant="default" className="bg-green-500">
          Completed
        </Badge>
      )
    }
    if (isFailed) {
      return <Badge variant="destructive">Failed</Badge>
    }
    return <Badge variant="secondary">Unknown</Badge>
  }

  const getStatusIcon = () => {
    if (isPending || isRunning) {
      return <Loader2 className="h-12 w-12 animate-spin text-blue-500" />
    }
    if (isComplete) {
      return <CheckCircle className="h-12 w-12 text-green-500" />
    }
    if (isFailed) {
      return <XCircle className="h-12 w-12 text-destructive" />
    }
    return <Clock className="h-12 w-12 text-muted-foreground" />
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-start justify-between">
          <div className="space-y-1">
            <CardTitle>Importing Repository</CardTitle>
            <CardDescription>{repositoryName}</CardDescription>
          </div>
          <Button
            variant="ghost"
            size="icon"
            onClick={onClose}
            disabled={isRunning || isPending}
          >
            <X className="h-4 w-4" />
          </Button>
        </div>
      </CardHeader>
      <CardContent className="space-y-6">
        {/* Status Icon */}
        <div className="flex flex-col items-center justify-center space-y-4">
          {getStatusIcon()}
          {getStatusBadge()}
        </div>

        {/* Progress Bar */}
        {(isRunning || isPending) && (
          <div className="space-y-2">
            <div className="flex items-center justify-between text-sm">
              <span className="text-muted-foreground">
                {status?.meta?.status || "Initializing..."}
              </span>
              <span className="font-medium">{progress}%</span>
            </div>
            <Progress value={progress} />
          </div>
        )}

        {/* Elapsed Time */}
        {(isRunning || isPending) && (
          <div className="flex items-center justify-center space-x-2 text-sm text-muted-foreground">
            <Clock className="h-4 w-4" />
            <span>Elapsed: {formatTime(elapsedTime)}</span>
          </div>
        )}

        {/* Success Result */}
        {isComplete && status?.result && (
          <div className="space-y-3 rounded-lg border bg-green-500/10 p-4">
            <div className="flex items-center space-x-2">
              <CheckCircle className="h-5 w-5 text-green-500" />
              <p className="font-medium">Import Successful</p>
            </div>
            <div className="grid grid-cols-2 gap-3 text-sm">
              {status.result.files_indexed !== undefined && (
                <div className="flex items-center space-x-2">
                  <FileText className="h-4 w-4 text-muted-foreground" />
                  <div>
                    <p className="text-muted-foreground">Files Indexed</p>
                    <p className="font-semibold">
                      {status.result.files_indexed.toLocaleString()}
                    </p>
                  </div>
                </div>
              )}
              {status.result.total_lines !== undefined && (
                <div className="flex items-center space-x-2">
                  <Code className="h-4 w-4 text-muted-foreground" />
                  <div>
                    <p className="text-muted-foreground">Total Lines</p>
                    <p className="font-semibold">
                      {status.result.total_lines.toLocaleString()}
                    </p>
                  </div>
                </div>
              )}
            </div>
          </div>
        )}

        {/* Error Result */}
        {isFailed && status?.error && (
          <div className="rounded-lg border border-destructive bg-destructive/10 p-4">
            <div className="flex items-start space-x-2">
              <XCircle className="h-5 w-5 text-destructive mt-0.5" />
              <div className="space-y-1">
                <p className="font-medium">Import Failed</p>
                <p className="text-sm text-muted-foreground">{status.error}</p>
              </div>
            </div>
          </div>
        )}

        {/* Job Info */}
        <div className="rounded-lg border bg-muted/50 p-3">
          <div className="grid grid-cols-2 gap-2 text-xs">
            <div>
              <p className="text-muted-foreground">Job ID</p>
              <p className="font-mono">{jobId.substring(0, 8)}...</p>
            </div>
            <div>
              <p className="text-muted-foreground">Status</p>
              <p className="font-medium">{status?.status || "Unknown"}</p>
            </div>
          </div>
        </div>

        {/* Actions */}
        <div className="flex justify-end space-x-2">
          {(isRunning || isPending) && (
            <Button
              variant="destructive"
              size="sm"
              onClick={handleCancel}
              disabled={isCancelling}
            >
              {isCancelling && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              Cancel
            </Button>
          )}
          {(isComplete || isFailed) && (
            <Button onClick={onClose}>Close</Button>
          )}
        </div>
      </CardContent>
    </Card>
  )
}
