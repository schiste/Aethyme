'use client'

import { formatDistanceToNow } from 'date-fns'
import { Clock, X } from 'lucide-react'

import { useRecentFiles } from '@/lib/hooks/use-recent-files'

interface RecentFilesPanelProps {
  repositoryId: string
  repositoryName: string
  onFileClick: (path: string) => void
}

export function RecentFilesPanel({ repositoryId, _repositoryName, onFileClick }: RecentFilesPanelProps) {
  const { recentFiles, removeRecentFile, _clearRecentFiles, getFileIcon } = useRecentFiles()

  // Filter files for this repository
  const filesForRepo = recentFiles.filter((file) => file.repository_id === repositoryId)

  if (filesForRepo.length === 0) {
    return null
  }

  return (
    <div className="border-t border-gray-700 bg-gray-800/50 p-4">
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center space-x-2">
          <Clock className="h-4 w-4 text-gray-400" />
          <h3 className="text-sm font-medium text-gray-300">Recently Viewed</h3>
        </div>
        <button
          onClick={() => {
            if (confirm('Clear recent files for this repository?')) {
              filesForRepo.forEach((file) => removeRecentFile(file.id))
            }
          }}
          className="text-xs text-gray-500 hover:text-gray-300"
        >
          Clear
        </button>
      </div>

      <div className="space-y-1 max-h-64 overflow-y-auto">
        {filesForRepo.map((file) => {
          const fileName = file.file_path.split('/').pop() || file.file_path
          const filePath = file.file_path.split('/').slice(0, -1).join('/')

          return (
            <div
              key={file.id}
              className="flex items-start space-x-2 p-2 rounded hover:bg-gray-700/50 cursor-pointer group transition-colors"
            >
              <span className="text-sm flex-shrink-0 mt-0.5">
                {getFileIcon(file.language)}
              </span>

              <div
                className="flex-1 min-w-0"
                onClick={() => onFileClick(file.file_path)}
              >
                <p className="text-sm font-medium text-gray-200 truncate">
                  {fileName}
                </p>
                {filePath && (
                  <p className="text-xs text-gray-500 truncate">
                    {filePath}
                  </p>
                )}
                <p className="text-xs text-gray-500">
                  {formatDistanceToNow(file.timestamp, { addSuffix: true })}
                </p>
              </div>

              <button
                onClick={(e) => {
                  e.stopPropagation()
                  removeRecentFile(file.id)
                }}
                className="p-1 rounded hover:bg-gray-600 text-gray-500 hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          )
        })}
      </div>
    </div>
  )
}
