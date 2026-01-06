'use client'

import React, { useCallback, useEffect,useState } from 'react'
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter'
import { vscDarkPlus } from 'react-syntax-highlighter/dist/esm/styles/prism'

import { type FileContentResponse,searchApi } from '@/lib/api/search'

interface CodePreviewProps {
  repositoryId: string
  filePath: string
  language: string
  highlights?: string[]
  maxLines?: number
  showLineNumbers?: boolean
}

export function CodePreview({
  repositoryId,
  filePath,
  language,
  highlights = [],
  maxLines = 100,
  showLineNumbers = true
}: CodePreviewProps) {
  const [content, setContent] = useState<string | null>(null)
  const [isLoading, setIsLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [fileInfo, setFileInfo] = useState<FileContentResponse | null>(null)

  const loadFileContent = useCallback(async () => {
    setIsLoading(true)
    setError(null)

    try {
      const response = await searchApi.getFileContent(repositoryId, filePath)
      setFileInfo(response)

      // Limit lines if needed
      const lines = response.content.split('\n')
      if (lines.length > maxLines) {
        setContent(lines.slice(0, maxLines).join('\n'))
      } else {
        setContent(response.content)
      }
    } catch (err: any) {
      const errorMsg = err.response?.data?.detail || err.message || 'Failed to load file content'
      setError(errorMsg)
    } finally {
      setIsLoading(false)
    }
  }, [repositoryId, filePath, maxLines])

  useEffect(() => {
    loadFileContent()
  }, [loadFileContent])

  if (isLoading) {
    return (
      <div className="border border-gray-700 rounded-lg p-4 bg-gray-900">
        <div className="flex items-center space-x-2">
          <div className="animate-spin h-4 w-4 border-2 border-blue-500 border-t-transparent rounded-full"></div>
          <span className="text-sm text-gray-400">Loading file content...</span>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="border border-red-800 rounded-lg p-4 bg-red-900/20">
        <p className="text-sm text-red-400">Error: {error}</p>
      </div>
    )
  }

  if (!content) {
    return (
      <div className="border border-gray-700 rounded-lg p-4 bg-gray-900">
        <p className="text-sm text-gray-400">No content available</p>
      </div>
    )
  }

  return (
    <div className="border border-gray-700 rounded-lg overflow-hidden bg-gray-900">
      {/* File header */}
      <div className="px-4 py-2 bg-gray-800 border-b border-gray-700 flex items-center justify-between">
        <div className="flex items-center space-x-3">
          <span className="text-sm font-mono text-gray-300">{filePath}</span>
          <span className="text-xs text-gray-500 uppercase px-2 py-1 bg-gray-700 rounded">
            {fileInfo?.language || language}
          </span>
        </div>
        {fileInfo && (
          <div className="flex items-center space-x-4 text-xs text-gray-500">
            <span>{fileInfo.line_count} lines</span>
            <span>{(fileInfo.size_bytes / 1024).toFixed(1)} KB</span>
          </div>
        )}
      </div>

      {/* Code content */}
      <div className="overflow-x-auto">
        <SyntaxHighlighter
          language={mapLanguageToHighlighter(language)}
          style={vscDarkPlus}
          showLineNumbers={showLineNumbers}
          wrapLines={true}
          lineProps={(lineNumber) => {
            const shouldHighlight = highlights.some(h =>
              content?.split('\n')[lineNumber - 1]?.includes(h)
            )
            return {
              style: shouldHighlight
                ? { backgroundColor: 'rgba(255, 212, 59, 0.15)' }
                : {}
            }
          }}
          customStyle={{
            margin: 0,
            padding: '1rem',
            fontSize: '0.875rem',
            background: 'transparent'
          }}
        >
          {content}
        </SyntaxHighlighter>
      </div>

      {fileInfo && fileInfo.line_count > maxLines && (
        <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-center">
          <span className="text-xs text-gray-500">
            Showing {maxLines} of {fileInfo.line_count} lines
          </span>
        </div>
      )}
    </div>
  )
}

/**
 * Map backend language names to syntax highlighter language keys
 */
function mapLanguageToHighlighter(language: string): string {
  const languageMap: Record<string, string> = {
    'Python': 'python',
    'JavaScript': 'javascript',
    'TypeScript': 'typescript',
    'Java': 'java',
    'C': 'c',
    'C++': 'cpp',
    'C#': 'csharp',
    'Go': 'go',
    'Rust': 'rust',
    'Ruby': 'ruby',
    'PHP': 'php',
    'Swift': 'swift',
    'Kotlin': 'kotlin',
    'Scala': 'scala',
    'R': 'r',
    'Shell': 'bash',
    'Bash': 'bash',
    'PowerShell': 'powershell',
    'SQL': 'sql',
    'HTML': 'html',
    'CSS': 'css',
    'SCSS': 'scss',
    'JSON': 'json',
    'YAML': 'yaml',
    'XML': 'xml',
    'Markdown': 'markdown',
    'Dockerfile': 'docker',
    'Makefile': 'makefile'
  }

  return languageMap[language] || language.toLowerCase()
}
