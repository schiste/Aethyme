'use client'

import { ThemeToggle } from '@/components/theme-toggle'

import { UserMenu } from './user-menu'

interface TopNavProps {
  title: string
  user?: {
    full_name: string
    email: string
  }
}

export function TopNav({ title, user }: TopNavProps) {
  return (
    <header className="h-16 border-b border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-950">
      <div className="h-full flex items-center justify-between px-6 lg:px-8">
        <div>
          <h2 className="text-2xl font-semibold text-slate-900 dark:text-white">
            {title}
          </h2>
        </div>

        <div className="flex items-center gap-4">
          <ThemeToggle />
          <UserMenu user={user} />
        </div>
      </div>
    </header>
  )
}
