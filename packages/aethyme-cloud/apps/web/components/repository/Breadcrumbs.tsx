'use client'

import React from 'react'

interface BreadcrumbsProps {
  repositoryName: string
  filePath?: string
  onNavigate: (path: string) => void
}

export function Breadcrumbs({ repositoryName, filePath, onNavigate }: BreadcrumbsProps) {
  if (!filePath) {
    return (
      <div className="flex items-center space-x-2 text-sm text-gray-400 py-2 px-4 bg-gray-800/50 border-b border-gray-700">
        <span className="font-medium text-gray-300">{repositoryName}</span>
      </div>
    )
  }

  const parts = filePath.split('/')
  const segments: Array<{ name: string; path: string }> = []

  // Build cumulative paths
  for (let i = 0; i < parts.length; i++) {
    segments.push({
      name: parts[i],
      path: parts.slice(0, i + 1).join('/')
    })
  }

  return (
    <div className="flex items-center space-x-2 text-sm text-gray-400 py-2 px-4 bg-gray-800/50 border-b border-gray-700 overflow-x-auto">
      {/* Repository root */}
      <button
        onClick={() => onNavigate('')}
        className="font-medium text-blue-400 hover:text-blue-300"
      >
        {repositoryName}
      </button>

      {/* Path segments */}
      {segments.map((segment, index) => {
        const isLast = index === segments.length - 1

        return (
          <React.Fragment key={segment.path}>
            <span className="text-gray-600">/</span>
            {isLast ? (
              <span className="text-gray-300 font-medium">{segment.name}</span>
            ) : (
              <button
                onClick={() => onNavigate(segment.path)}
                className="text-blue-400 hover:text-blue-300"
              >
                {segment.name}
              </button>
            )}
          </React.Fragment>
        )
      })}
    </div>
  )
}
