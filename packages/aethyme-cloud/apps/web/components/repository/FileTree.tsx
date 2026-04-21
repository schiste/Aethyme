'use client'

import { Search, X } from 'lucide-react'
import React, { useCallback, useMemo,useState } from 'react'

import { type FileTreeNode } from '@/lib/api/repositories'

interface FileTreeProps {
  tree: FileTreeNode[]
  currentPath?: string
  onFileClick: (path: string) => void
}

interface FileTreeNodeProps {
  node: FileTreeNode
  level: number
  currentPath?: string
  onFileClick: (path: string) => void
  searchQuery?: string
  forceExpanded?: boolean
}

function FileTreeNodeComponent({ node, level, currentPath, onFileClick, searchQuery = '', forceExpanded = false }: FileTreeNodeProps) {
  const [isExpanded, setIsExpanded] = useState(level === 0) // Root level expanded by default
  const isFolder = node.type === 'folder'
  const isActive = currentPath === node.path
  const children = node.children ?? []
  const hasChildren = children.length > 0

  // Auto-expand if search query matches or forceExpanded
  const shouldExpand =
    forceExpanded || (searchQuery.length > 0 && node.name.toLowerCase().includes(searchQuery.toLowerCase()))
  const expanded = isExpanded || shouldExpand

  // Check if node matches search
  const matchesSearch = !searchQuery || node.name.toLowerCase().includes(searchQuery.toLowerCase())

  const handleClick = () => {
    if (isFolder) {
      setIsExpanded(!isExpanded)
    } else {
      onFileClick(node.path)
    }
  }

  const getFileIcon = (language?: string | null) => {
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

  // Don't render if doesn't match search (unless it's a folder with matching children)
  if (!matchesSearch && !isFolder) {
    return null
  }

  // Highlight matching text
  const highlightMatch = (text: string) => {
    if (!searchQuery) return <span>{text}</span>

    const index = text.toLowerCase().indexOf(searchQuery.toLowerCase())
    if (index === -1) return <span>{text}</span>

    return (
      <span>
        {text.substring(0, index)}
        <span className="bg-yellow-500/30 text-yellow-200">
          {text.substring(index, index + searchQuery.length)}
        </span>
        {text.substring(index + searchQuery.length)}
      </span>
    )
  }

  return (
    <div>
      <div
        onClick={handleClick}
        className={`flex items-center px-2 py-1 cursor-pointer hover:bg-gray-700/50 rounded ${
          isActive ? 'bg-blue-600/30 text-blue-300' : 'text-gray-300'
        }`}
        style={{ paddingLeft: `${level * 12 + 8}px` }}
      >
        {isFolder && (
          <span className="mr-1 text-xs">
            {expanded ? '▼' : '▶'}
          </span>
        )}
        <span className="mr-2">
          {isFolder ? (expanded ? '📂' : '📁') : getFileIcon(node.language)}
        </span>
        <span className="text-sm truncate flex-1">{highlightMatch(node.name)}</span>
        {!isFolder && node.line_count && (
          <span className="text-xs text-gray-500 ml-2">{node.line_count}</span>
        )}
      </div>

      {isFolder && expanded && hasChildren && (
        <div>
          {children.map((child, index) => (
            <FileTreeNodeComponent
              key={`${child.path}-${index}`}
              node={child}
              level={level + 1}
              currentPath={currentPath}
              onFileClick={onFileClick}
              searchQuery={searchQuery}
              forceExpanded={shouldExpand}
            />
          ))}
        </div>
      )}
    </div>
  )
}

export function FileTree({ tree, currentPath, onFileClick }: FileTreeProps) {
  const [searchQuery, setSearchQuery] = useState('')

  // Count matches
  const countMatches = useCallback((nodes: FileTreeNode[], query: string): number => {
    if (!query) return 0
    let count = 0
    for (const node of nodes) {
      if (node.name.toLowerCase().includes(query.toLowerCase())) {
        count++
      }
      if (node.children) {
        count += countMatches(node.children, query)
      }
    }
    return count
  }, [])

  const matchCount = useMemo(() => countMatches(tree, searchQuery), [tree, searchQuery, countMatches])

  if (tree.length === 0) {
    return (
      <div className="p-4 text-center text-gray-500 text-sm">
        <p>No files found</p>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full bg-gray-900 border-r border-gray-700">
      {/* Search Input */}
      <div className="p-3 border-b border-gray-700">
        <div className="relative">
          <Search className="absolute left-3 top-1/2 transform -translate-y-1/2 h-4 w-4 text-gray-500" />
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Filter files..."
            className="w-full pl-9 pr-8 py-2 bg-gray-800 border border-gray-700 rounded text-sm text-gray-300 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
          />
          {searchQuery && (
            <button
              onClick={() => setSearchQuery('')}
              className="absolute right-2 top-1/2 transform -translate-y-1/2 text-gray-500 hover:text-gray-300"
            >
              <X className="h-4 w-4" />
            </button>
          )}
        </div>
        {searchQuery && (
          <p className="mt-2 text-xs text-gray-500">
            {matchCount} {matchCount === 1 ? 'match' : 'matches'}
          </p>
        )}
      </div>

      {/* File Tree */}
      <div className="flex-1 overflow-y-auto">
        <div className="py-2">
          {tree.map((node, index) => (
            <FileTreeNodeComponent
              key={`${node.path}-${index}`}
              node={node}
              level={0}
              currentPath={currentPath}
              onFileClick={onFileClick}
              searchQuery={searchQuery}
            />
          ))}
        </div>
      </div>
    </div>
  )
}
