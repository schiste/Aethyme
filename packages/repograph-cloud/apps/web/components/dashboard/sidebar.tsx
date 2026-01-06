'use client'

import { BarChart3, FileText,FolderGit2, GitBranch, Home, Key, Menu, Network, Search, Settings, Sparkles, X } from 'lucide-react'
import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { useState } from 'react'

import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import { cn } from '@/lib/utils'

const navigation = [
  { name: 'Overview', href: '/dashboard', icon: Home },
  { name: 'Search', href: '/search', icon: Search },
  { name: 'Repositories', href: '/dashboard/repositories', icon: FolderGit2 },
  { name: 'API Keys', href: '/dashboard/api-keys', icon: Key },
]

const graphNavigation = [
  { name: 'Code Graph', href: '/graph', icon: Network },
  { name: 'Dependencies', href: '/graph/dependencies', icon: GitBranch },
]

const docsNavigation = [
  { name: 'Documentation', href: '/docs', icon: FileText },
  { name: 'Metrics', href: '/docs#metrics', icon: BarChart3 },
]

const aiNavigation = [
  { name: 'Semantic Search', href: '/search/semantic', icon: Sparkles },
  { name: 'AI Settings', href: '/dashboard/settings/ai', icon: Settings },
  { name: 'Usage & Costs', href: '/dashboard/settings/ai/usage', icon: BarChart3 },
]

const settingsNavigation = [
  { name: 'Settings', href: '/dashboard/settings', icon: Settings },
]

export function Sidebar() {
  const pathname = usePathname()
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false)

  return (
    <>
      {/* Mobile menu button */}
      <div className="lg:hidden fixed top-4 left-4 z-50">
        <Button
          variant="outline"
          size="icon"
          onClick={() => setMobileMenuOpen(!mobileMenuOpen)}
        >
          {mobileMenuOpen ? <X className="h-5 w-5" /> : <Menu className="h-5 w-5" />}
        </Button>
      </div>

      {/* Sidebar */}
      <aside
        className={cn(
          'fixed inset-y-0 left-0 z-40 w-64 bg-slate-50 dark:bg-slate-900 border-r border-slate-200 dark:border-slate-800 transition-transform duration-200 ease-in-out lg:translate-x-0',
          mobileMenuOpen ? 'translate-x-0' : '-translate-x-full'
        )}
      >
        <div className="flex flex-col h-full">
          {/* Logo */}
          <div className="flex items-center h-16 px-6 border-b border-slate-200 dark:border-slate-800">
            <h1 className="text-xl font-bold text-slate-900 dark:text-white">
              RepoGraph
            </h1>
          </div>

          {/* Navigation */}
          <nav className="flex-1 px-4 py-6 space-y-4 overflow-y-auto">
            <div className="space-y-1">
              {navigation.map((item) => {
                const isActive = pathname === item.href
                return (
                  <Link
                    key={item.name}
                    href={item.href}
                    onClick={() => setMobileMenuOpen(false)}
                    className={cn(
                      'flex items-center gap-3 px-3 py-2 text-sm font-medium rounded-lg transition-colors',
                      isActive
                        ? 'bg-primary text-primary-foreground'
                        : 'text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800'
                    )}
                  >
                    <item.icon className="h-5 w-5" />
                    {item.name}
                  </Link>
                )
              })}
            </div>

            <div>
              <div className="px-3 mb-2">
                <h3 className="text-xs font-semibold text-slate-500 dark:text-slate-400 uppercase tracking-wider">
                  Graph Analysis
                </h3>
              </div>
              <div className="space-y-1">
                {graphNavigation.map((item) => {
                  const isActive = pathname?.startsWith(item.href)
                  return (
                    <Link
                      key={item.name}
                      href={item.href}
                      onClick={() => setMobileMenuOpen(false)}
                      className={cn(
                        'flex items-center gap-3 px-3 py-2 text-sm font-medium rounded-lg transition-colors',
                        isActive
                          ? 'bg-blue-500/10 text-blue-700 dark:text-blue-400'
                          : 'text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800'
                      )}
                    >
                      <item.icon className="h-5 w-5" />
                      {item.name}
                    </Link>
                  )
                })}
              </div>
            </div>

            <div>
              <div className="px-3 mb-2">
                <h3 className="text-xs font-semibold text-slate-500 dark:text-slate-400 uppercase tracking-wider">
                  Documentation
                </h3>
              </div>
              <div className="space-y-1">
                {docsNavigation.map((item) => {
                  const isActive = pathname?.startsWith(item.href.split('#')[0])
                  return (
                    <Link
                      key={item.name}
                      href={item.href}
                      onClick={() => setMobileMenuOpen(false)}
                      className={cn(
                        'flex items-center gap-3 px-3 py-2 text-sm font-medium rounded-lg transition-colors',
                        isActive
                          ? 'bg-green-500/10 text-green-700 dark:text-green-400'
                          : 'text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800'
                      )}
                    >
                      <item.icon className="h-5 w-5" />
                      {item.name}
                    </Link>
                  )
                })}
              </div>
            </div>

            <div>
              <div className="px-3 mb-2">
                <h3 className="text-xs font-semibold text-slate-500 dark:text-slate-400 uppercase tracking-wider">
                  AI Features
                </h3>
              </div>
              <div className="space-y-1">
                {aiNavigation.map((item) => {
                  const isActive = pathname === item.href
                  return (
                    <Link
                      key={item.name}
                      href={item.href}
                      onClick={() => setMobileMenuOpen(false)}
                      className={cn(
                        'flex items-center gap-3 px-3 py-2 text-sm font-medium rounded-lg transition-colors',
                        isActive
                          ? 'bg-purple-500/10 text-purple-700 dark:text-purple-400'
                          : 'text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800'
                      )}
                    >
                      <item.icon className="h-5 w-5" />
                      {item.name}
                    </Link>
                  )
                })}
              </div>
            </div>

            <Separator />

            <div className="space-y-1">
              {settingsNavigation.map((item) => {
                const isActive = pathname === item.href
                return (
                  <Link
                    key={item.name}
                    href={item.href}
                    onClick={() => setMobileMenuOpen(false)}
                    className={cn(
                      'flex items-center gap-3 px-3 py-2 text-sm font-medium rounded-lg transition-colors',
                      isActive
                        ? 'bg-primary text-primary-foreground'
                        : 'text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800'
                    )}
                  >
                    <item.icon className="h-5 w-5" />
                    {item.name}
                  </Link>
                )
              })}
            </div>
          </nav>

          {/* Footer */}
          <div className="p-4 border-t border-slate-200 dark:border-slate-800">
            <p className="text-xs text-slate-500 dark:text-slate-400 text-center">
              RepoGraph Cloud v1.0
            </p>
          </div>
        </div>
      </aside>

      {/* Mobile overlay */}
      {mobileMenuOpen && (
        <div
          className="fixed inset-0 z-30 bg-black/50 lg:hidden"
          onClick={() => setMobileMenuOpen(false)}
        />
      )}
    </>
  )
}
