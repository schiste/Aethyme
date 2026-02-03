'use client'

import { useParams, useRouter,useSearchParams } from 'next/navigation'
import React, { useEffect,useState } from 'react'

import { CodePreview } from '@/components/code/CodePreview'
import { Breadcrumbs } from '@/components/repository/Breadcrumbs'
import { FileTree } from '@/components/repository/FileTree'
import { RecentFilesPanel } from '@/components/repository/RecentFilesPanel'
import { repositoriesApi, type RepositoryStatsResponse, type RepositoryTreeResponse } from '@/lib/api/repositories'
import { useRecentFiles } from '@/lib/hooks/use-recent-files'

export default function RepositoryPage() {
  const params = useParams()
  const searchParams = useSearchParams()
  const router = useRouter()
  const repositoryId = params.id as string
  const currentPath = searchParams.get('path') || ''
  const { addRecentFile } = useRecentFiles()

  const [stats, setStats] = useState<RepositoryStatsResponse | null>(null)
  const [tree, setTree] = useState<RepositoryTreeResponse | null>(null)
  const [isLoadingStats, setIsLoadingStats] = useState(true)
  const [isLoadingTree, setIsLoadingTree] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const loadStats = React.useCallback(async () => {
    setIsLoadingStats(true)
    setError(null)
    try {
      const data = await repositoriesApi.getStats(repositoryId)
      setStats(data)
    } catch (err: any) {
      const errorMsg = err.response?.data?.detail || err.message || 'Failed to load repository stats'
      setError(errorMsg)
    } finally {
      setIsLoadingStats(false)
    }
  }, [repositoryId])

  const loadTree = React.useCallback(async () => {
    setIsLoadingTree(true)
    setError(null)
    try {
      const data = await repositoriesApi.getTree(repositoryId)
      setTree(data)
    } catch (err: any) {
      const errorMsg = err.response?.data?.detail || err.message || 'Failed to load file tree'
      setError(errorMsg)
    } finally {
      setIsLoadingTree(false)
    }
  }, [repositoryId])

  // Load repository stats
  useEffect(() => {
    loadStats()
  }, [loadStats])

  // Load file tree
  useEffect(() => {
    loadTree()
  }, [loadTree])

  const handleFileClick = (path: string) => {
    router.push(`/repositories/${repositoryId}?path=${encodeURIComponent(path)}`)
  }

  // Track file view
  useEffect(() => {
    if (currentPath && stats && tree) {
      // Get file language
      const findFileInTree = (nodes: any[], path: string): any => {
        for (const node of nodes) {
          if (node.path === path && node.type === 'file') {
            return node
          }
          if (node.children) {
            const found = findFileInTree(node.children, path)
            if (found) return found
          }
        }
        return null
      }

      const file = findFileInTree(tree.tree, currentPath)
      if (file) {
        addRecentFile(repositoryId, stats.name, currentPath, file.language)
      }
    }
  }, [currentPath, repositoryId, stats, tree, addRecentFile])

  const handleBreadcrumbNavigate = (path: string) => {
    if (path) {
      router.push(`/repositories/${repositoryId}?path=${encodeURIComponent(path)}`)
    } else {
      router.push(`/repositories/${repositoryId}`)
    }
  }

  const formatNumber = (num: number) => {
    return num.toLocaleString()
  }

  const formatDate = (dateStr?: string | null) => {
    if (!dateStr) return 'Never'
    const date = new Date(dateStr)
    const now = new Date()
    const diffMs = now.getTime() - date.getTime()
    const diffMins = Math.floor(diffMs / 60000)
    const diffHours = Math.floor(diffMs / 3600000)
    const diffDays = Math.floor(diffMs / 86400000)

    if (diffMins < 1) return 'Just now'
    if (diffMins < 60) return `${diffMins} minute${diffMins > 1 ? 's' : ''} ago`
    if (diffHours < 24) return `${diffHours} hour${diffHours > 1 ? 's' : ''} ago`
    if (diffDays < 7) return `${diffDays} day${diffDays > 1 ? 's' : ''} ago`
    return date.toLocaleDateString()
  }

  // Get current file language
  const getCurrentFileLanguage = (): string => {
    if (!currentPath || !tree) return ''

    const findFileInTree = (nodes: any[], path: string): any => {
      for (const node of nodes) {
        if (node.path === path && node.type === 'file') {
          return node
        }
        if (node.children) {
          const found = findFileInTree(node.children, path)
          if (found) return found
        }
      }
      return null
    }

    const file = findFileInTree(tree.tree, currentPath)
    return file?.language || ''
  }

  if (error) {
    return (
      <div className="min-h-screen bg-gray-950 flex items-center justify-center">
        <div className="bg-red-900/20 border border-red-800 rounded-lg p-6 max-w-md">
          <h2 className="text-lg font-semibold text-red-400 mb-2">Error Loading Repository</h2>
          <p className="text-sm text-red-300">{error}</p>
        </div>
      </div>
    )
  }

  if (isLoadingStats || isLoadingTree) {
    return (
      <div className="min-h-screen bg-gray-950 flex items-center justify-center">
        <div className="flex items-center space-x-3">
          <div className="animate-spin h-8 w-8 border-4 border-blue-500 border-t-transparent rounded-full"></div>
          <span className="text-lg text-gray-400">Loading repository...</span>
        </div>
      </div>
    )
  }

  if (!stats || !tree) {
    return (
      <div className="min-h-screen bg-gray-950 flex items-center justify-center">
        <p className="text-gray-500">Repository not found</p>
      </div>
    )
  }

  return (
    <div className="min-h-screen bg-gray-950">
      {/* Header */}
      <div className="border-b border-gray-800 bg-gray-900/50 backdrop-blur">
        <div className="px-6 py-4">
          <div className="flex items-start justify-between mb-3">
            <div>
              <h1 className="text-2xl font-bold text-white mb-1">{stats.name}</h1>
              <p className="text-sm text-gray-500">{stats.full_name}</p>
              {stats.description && (
                <p className="text-sm text-gray-400 mt-2">{stats.description}</p>
              )}
            </div>
            <a
              href={stats.url}
              target="_blank"
              rel="noopener noreferrer"
              className="px-4 py-2 bg-gray-800 hover:bg-gray-700 text-gray-300 rounded-lg text-sm border border-gray-700"
            >
              View on GitHub →
            </a>
          </div>

          {/* Stats */}
          <div className="flex items-center space-x-6 text-sm text-gray-400">
            <span className="flex items-center space-x-1">
              <span>⭐</span>
              <span>{formatNumber(stats.stars)}</span>
            </span>
            <span className="flex items-center space-x-1">
              <span>🔄</span>
              <span>{formatNumber(stats.forks)}</span>
            </span>
            {stats.primary_language && (
              <span className="px-2 py-1 bg-blue-600/20 text-blue-400 rounded text-xs uppercase">
                {stats.primary_language}
              </span>
            )}
            <span>{formatNumber(stats.total_files)} files</span>
            <span>{formatNumber(stats.total_lines)} lines</span>
            <span className="text-gray-500">
              Last indexed: {formatDate(stats.indexed_at)}
            </span>
          </div>

          {/* Languages */}
          {Object.keys(stats.languages).length > 0 && (
            <div className="mt-3 flex items-center space-x-2 text-xs">
              {Object.entries(stats.languages)
                .sort(([, a], [, b]) => b - a)
                .slice(0, 5)
                .map(([lang, lines]) => (
                  <span
                    key={lang}
                    className="px-2 py-1 bg-gray-800 text-gray-400 rounded"
                  >
                    {lang}: {formatNumber(lines)} lines
                  </span>
                ))}
            </div>
          )}
        </div>
      </div>

      {/* Main content */}
      <div className="flex h-[calc(100vh-180px)]">
        {/* File tree sidebar */}
        <div className="w-64 border-r border-gray-800 overflow-hidden flex flex-col">
          <div className="px-4 py-2 bg-gray-800 border-b border-gray-700">
            <h2 className="text-sm font-semibold text-gray-300">Files</h2>
            <p className="text-xs text-gray-500 mt-1">
              {tree.total_folders} folders, {tree.total_files} files
            </p>
          </div>
          <div className="flex-1 overflow-auto">
            <FileTree
              tree={tree.tree}
              currentPath={currentPath}
              onFileClick={handleFileClick}
            />
          </div>

          {/* Recent Files Panel */}
          <RecentFilesPanel
            repositoryId={repositoryId}
            repositoryName={stats.name}
            onFileClick={handleFileClick}
          />
        </div>

        {/* File viewer */}
        <div className="flex-1 overflow-auto">
          {currentPath ? (
            <div>
              <Breadcrumbs
                repositoryName={stats.name}
                filePath={currentPath}
                onNavigate={handleBreadcrumbNavigate}
              />
              <div className="p-4">
                <CodePreview
                  repositoryId={repositoryId}
                  filePath={currentPath}
                  language={getCurrentFileLanguage()}
                  maxLines={1000}
                />
              </div>
            </div>
          ) : (
            <div className="flex items-center justify-center h-full">
              <div className="text-center">
                <svg
                  className="mx-auto h-12 w-12 text-gray-600"
                  fill="none"
                  viewBox="0 0 24 24"
                  stroke="currentColor"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"
                  />
                </svg>
                <h3 className="mt-4 text-lg font-medium text-gray-400">Select a file</h3>
                <p className="mt-2 text-sm text-gray-500">
                  Choose a file from the tree to view its contents
                </p>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
