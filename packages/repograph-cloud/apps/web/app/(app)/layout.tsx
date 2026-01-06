'use client'

import { useRouter } from 'next/navigation'
import { type ReactNode, useEffect, useState } from 'react'

import { Sidebar } from '@/components/dashboard/sidebar'
import { TopNav } from '@/components/dashboard/top-nav'
import { KeyboardShortcutsDialog } from '@/components/ui/KeyboardShortcutsDialog'
import apiClient from '@/lib/api-client'
import { useKeyboardShortcuts } from '@/lib/hooks/use-keyboard-shortcuts'

interface User {
  id: string
  email: string
  full_name: string
}

export default function AppLayout({ children }: { children: ReactNode }) {
  const router = useRouter()
  const [user, setUser] = useState<User | null>(null)
  const [loading, setLoading] = useState(true)
  const [showShortcuts, setShowShortcuts] = useState(false)
  const [_sidebarOpen, setSidebarOpen] = useState(true)

  useEffect(() => {
    const fetchUser = async () => {
      const token = localStorage.getItem('access_token')

      if (!token) {
        router.push('/login')
        return
      }

      try {
        const { data } = await apiClient.get('/users/me')
        setUser(data)
      } catch (error) {
        console.error('Failed to fetch user:', error)
        router.push('/login')
      } finally {
        setLoading(false)
      }
    }

    fetchUser()
  }, [router])

  // Global keyboard shortcuts
  useKeyboardShortcuts({
    shortcuts: [
      {
        key: 'k',
        meta: true,
        handler: () => {
          // Focus search - navigate to search page and focus input
          router.push('/search')
        },
        description: 'Focus search',
        category: 'Navigation'
      },
      {
        key: '/',
        meta: true,
        handler: () => {
          setShowShortcuts(true)
        },
        description: 'Show keyboard shortcuts',
        category: 'Help'
      },
      {
        key: 'escape',
        handler: () => {
          setShowShortcuts(false)
        },
        description: 'Close dialog',
        category: 'General'
      },
      {
        key: 'b',
        meta: true,
        handler: () => {
          setSidebarOpen((prev) => !prev)
        },
        description: 'Toggle sidebar',
        category: 'Navigation'
      }
    ]
  })

  if (loading) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto"></div>
          <p className="mt-4 text-sm text-slate-600 dark:text-slate-400">Loading...</p>
        </div>
      </div>
    )
  }

  return (
    <div className="min-h-screen bg-slate-50 dark:bg-slate-950">
      <Sidebar />

      <div className="lg:pl-64">
        <TopNav title="Dashboard" user={user || undefined} />

        <main className="p-6 lg:p-8">
          {children}
        </main>
      </div>

      <KeyboardShortcutsDialog
        open={showShortcuts}
        onOpenChange={setShowShortcuts}
      />
    </div>
  )
}
