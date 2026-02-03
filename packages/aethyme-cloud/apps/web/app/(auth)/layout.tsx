import { type ReactNode } from 'react'

export default function AuthLayout({ children }: { children: ReactNode }) {
  return (
    <div className="min-h-screen flex items-center justify-center bg-gradient-to-br from-slate-50 to-slate-100 dark:from-slate-900 dark:to-slate-800">
      <div className="w-full max-w-md px-4">
        <div className="mb-8 text-center">
          <h1 className="text-3xl font-bold text-slate-900 dark:text-white">
            Aethyme Cloud
          </h1>
          <p className="mt-2 text-sm text-slate-600 dark:text-slate-400">
            Code Intelligence Platform
          </p>
        </div>
        {children}
      </div>
    </div>
  )
}
