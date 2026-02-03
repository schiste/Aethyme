import './globals.css'

import type { Metadata } from 'next'
import { Inter } from 'next/font/google'

import { ErrorBoundary } from '@/components/ErrorBoundary'

import { Providers } from './providers'

const inter = Inter({ subsets: ['latin'] })

export const metadata: Metadata = {
  title: 'Aethyme Cloud - Code Intelligence Platform',
  description: 'Hosted code intelligence for developers and teams',
}

export default function RootLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <html lang="en" suppressHydrationWarning>
      <body className={inter.className}>
        <ErrorBoundary>
          <Providers>{children}</Providers>
        </ErrorBoundary>
      </body>
    </html>
  )
}
