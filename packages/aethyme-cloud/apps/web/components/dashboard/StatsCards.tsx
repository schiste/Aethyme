'use client'

import { AlertCircle, Code2, FileText, FolderGit2, Loader2 } from 'lucide-react'
import { useEffect, useState } from 'react'

import { type DashboardStats,getDashboardStats } from '@/lib/api/dashboard'

interface StatCardProps {
  title: string
  value: string | number
  subtitle?: string
  icon: React.ReactNode
  variant?: 'default' | 'success' | 'warning' | 'error'
}

function StatCard({ title, value, subtitle, icon, variant = 'default' }: StatCardProps) {
  const variantStyles = {
    default: 'bg-slate-100 dark:bg-slate-800 border-slate-200 dark:border-slate-700',
    success: 'bg-green-50 dark:bg-green-900/20 border-green-200 dark:border-green-800',
    warning: 'bg-yellow-50 dark:bg-yellow-900/20 border-yellow-200 dark:border-yellow-800',
    error: 'bg-red-50 dark:bg-red-900/20 border-red-200 dark:border-red-800',
  }

  return (
    <div className={`p-6 rounded-lg border ${variantStyles[variant]}`}>
      <div className="flex items-center justify-between mb-2">
        <h3 className="text-sm font-medium text-slate-600 dark:text-slate-400">{title}</h3>
        <div className="text-slate-500 dark:text-slate-400">{icon}</div>
      </div>
      <div className="text-2xl font-bold text-slate-900 dark:text-white mb-1">
        {typeof value === 'number' ? value.toLocaleString() : value}
      </div>
      {subtitle && (
        <p className="text-xs text-slate-500 dark:text-slate-400">{subtitle}</p>
      )}
    </div>
  )
}

interface LanguageBreakdownProps {
  languages: DashboardStats['languages']
}

function LanguageBreakdown({ languages }: LanguageBreakdownProps) {
  const getLanguageColor = (language: string): string => {
    const colors: Record<string, string> = {
      TypeScript: '#3178c6',
      JavaScript: '#f7df1e',
      Python: '#3776ab',
      Java: '#007396',
      Go: '#00add8',
      Rust: '#dea584',
      Ruby: '#cc342d',
      PHP: '#777bb4',
      'C++': '#00599c',
      'C#': '#239120',
      Swift: '#fa7343',
      Kotlin: '#7f52ff',
      HTML: '#e34c26',
      CSS: '#1572b6',
    }
    return colors[language] || '#6b7280'
  }

  if (languages.length === 0) {
    return (
      <div className="p-6 rounded-lg border bg-slate-100 dark:bg-slate-800 border-slate-200 dark:border-slate-700">
        <h3 className="text-sm font-medium text-slate-600 dark:text-slate-400 mb-4">
          Languages
        </h3>
        <p className="text-sm text-slate-500 dark:text-slate-400">
          No language data available yet
        </p>
      </div>
    )
  }

  return (
    <div className="p-6 rounded-lg border bg-slate-100 dark:bg-slate-800 border-slate-200 dark:border-slate-700">
      <h3 className="text-sm font-medium text-slate-600 dark:text-slate-400 mb-4">
        Languages
      </h3>

      {/* Horizontal bar chart */}
      <div className="space-y-3 mb-4">
        {languages.slice(0, 5).map((lang) => (
          <div key={lang.language}>
            <div className="flex items-center justify-between mb-1">
              <span className="text-sm font-medium text-slate-700 dark:text-slate-300">
                {lang.language}
              </span>
              <span className="text-xs text-slate-500 dark:text-slate-400">
                {lang.percentage.toFixed(1)}%
              </span>
            </div>
            <div className="w-full bg-slate-200 dark:bg-slate-700 rounded-full h-2">
              <div
                className="h-2 rounded-full transition-all duration-500"
                style={{
                  width: `${lang.percentage}%`,
                  backgroundColor: getLanguageColor(lang.language),
                }}
              />
            </div>
            <p className="text-xs text-slate-500 dark:text-slate-400 mt-1">
              {lang.file_count.toLocaleString()} files • {lang.line_count.toLocaleString()} lines
            </p>
          </div>
        ))}
      </div>

      {/* Additional languages */}
      {languages.length > 5 && (
        <div className="pt-3 border-t border-slate-200 dark:border-slate-700">
          <p className="text-xs text-slate-500 dark:text-slate-400">
            + {languages.length - 5} more language{languages.length - 5 > 1 ? 's' : ''}
          </p>
        </div>
      )}
    </div>
  )
}

export function StatsCards() {
  const [stats, setStats] = useState<DashboardStats | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const fetchStats = async () => {
      try {
        const data = await getDashboardStats()
        setStats(data)
      } catch (err) {
        console.error('Failed to fetch dashboard stats:', err)
        setError('Failed to load statistics')
      } finally {
        setLoading(false)
      }
    }

    fetchStats()
  }, [])

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-8 w-8 animate-spin text-slate-400" />
      </div>
    )
  }

  if (error || !stats) {
    return (
      <div className="p-6 rounded-lg border bg-red-50 dark:bg-red-900/20 border-red-200 dark:border-red-800">
        <p className="text-sm text-red-600 dark:text-red-400">{error || 'Failed to load statistics'}</p>
      </div>
    )
  }

  const indexingPercentage =
    stats.total_repositories > 0
      ? Math.round((stats.indexed_repositories / stats.total_repositories) * 100)
      : 0

  return (
    <div className="space-y-6">
      {/* Top row: Main stats */}
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
        <StatCard
          title="Repositories"
          value={stats.total_repositories}
          subtitle={`${stats.indexed_repositories} indexed (${indexingPercentage}%)`}
          icon={<FolderGit2 className="h-5 w-5" />}
          variant="default"
        />

        <StatCard
          title="Total Files"
          value={stats.total_files}
          subtitle="Indexed across all repositories"
          icon={<FileText className="h-5 w-5" />}
          variant="success"
        />

        <StatCard
          title="Lines of Code"
          value={stats.total_lines}
          subtitle="Total lines indexed"
          icon={<Code2 className="h-5 w-5" />}
          variant="success"
        />

        <StatCard
          title="Indexing Queue"
          value={stats.indexing_repositories + stats.failed_repositories}
          subtitle={`${stats.indexing_repositories} in progress, ${stats.failed_repositories} failed`}
          icon={<AlertCircle className="h-5 w-5" />}
          variant={
            stats.failed_repositories > 0
              ? 'warning'
              : stats.indexing_repositories > 0
              ? 'default'
              : 'success'
          }
        />
      </div>

      {/* Bottom row: Language breakdown */}
      <LanguageBreakdown languages={stats.languages} />
    </div>
  )
}
