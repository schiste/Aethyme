import { useCallback,useEffect, useState } from 'react'

export interface RecentFile {
  id: string
  repository_id: string
  repository_name: string
  file_path: string
  language?: string
  timestamp: Date
}

const STORAGE_KEY = 'repograph_recent_files'
const MAX_RECENT_FILES = 20

/**
 * Hook for tracking recently viewed files
 * Stores file views in localStorage for quick access
 */
export function useRecentFiles() {
  const [recentFiles, setRecentFiles] = useState<RecentFile[]>([])

  // Load recent files from localStorage on mount
  useEffect(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY)
      if (stored) {
        const parsed = JSON.parse(stored)
        // Convert timestamp strings back to Date objects
        const filesWithDates = parsed.map((file: any) => ({
          ...file,
          timestamp: new Date(file.timestamp)
        }))
        setRecentFiles(filesWithDates)
      }
    } catch (error) {
      console.error('Failed to load recent files:', error)
    }
  }, [])

  // Save recent files to localStorage whenever they change
  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(recentFiles))
    } catch (error) {
      console.error('Failed to save recent files:', error)
    }
  }, [recentFiles])

  const addRecentFile = useCallback((
    repositoryId: string,
    repositoryName: string,
    filePath: string,
    language?: string
  ) => {
    setRecentFiles((prev) => {
      // Check if this file already exists
      const existingIndex = prev.findIndex(
        (file) => file.repository_id === repositoryId && file.file_path === filePath
      )

      // If it exists, move it to the top with updated timestamp
      if (existingIndex >= 0) {
        const updated = [...prev]
        updated.splice(existingIndex, 1)
        return [
          {
            id: crypto.randomUUID(),
            repository_id: repositoryId,
            repository_name: repositoryName,
            file_path: filePath,
            language,
            timestamp: new Date()
          },
          ...updated
        ].slice(0, MAX_RECENT_FILES)
      }

      // Add new file to the beginning
      return [
        {
          id: crypto.randomUUID(),
          repository_id: repositoryId,
          repository_name: repositoryName,
          file_path: filePath,
          language,
          timestamp: new Date()
        },
        ...prev
      ].slice(0, MAX_RECENT_FILES)
    })
  }, [])

  const removeRecentFile = useCallback((id: string) => {
    setRecentFiles((prev) => prev.filter((file) => file.id !== id))
  }, [])

  const clearRecentFiles = useCallback(() => {
    setRecentFiles([])
    localStorage.removeItem(STORAGE_KEY)
  }, [])

  const getRecentFilesForRepository = useCallback((repositoryId: string) => {
    return recentFiles.filter((file) => file.repository_id === repositoryId)
  }, [recentFiles])

  const getFileIcon = (language?: string) => {
    if (!language) return '📄'

    const iconMap: Record<string, string> = {
      'Python': '🐍',
      'JavaScript': '📜',
      'TypeScript': '📘',
      'Java': '☕',
      'Go': '🔷',
      'Rust': '🦀',
      'Ruby': '💎',
      'PHP': '🐘',
      'C': '©️',
      'C++': '➕',
      'C#': '#️⃣',
      'HTML': '🌐',
      'CSS': '🎨',
      'JSON': '📋',
      'YAML': '📝',
      'Markdown': '📖',
      'Shell': '🐚',
      'Bash': '🐚'
    }

    return iconMap[language] || '📄'
  }

  return {
    recentFiles,
    addRecentFile,
    removeRecentFile,
    clearRecentFiles,
    getRecentFilesForRepository,
    getFileIcon
  }
}
