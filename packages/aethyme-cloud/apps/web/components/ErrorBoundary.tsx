'use client'

import { AlertTriangle, Home,RefreshCw } from 'lucide-react'
import React, { Component, type ReactNode } from 'react'

import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

interface Props {
  children: ReactNode
  fallback?: ReactNode
  onError?: (error: Error, errorInfo: React.ErrorInfo) => void
}

interface State {
  hasError: boolean
  error: Error | null
  errorInfo: React.ErrorInfo | null
}

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props)
    this.state = {
      hasError: false,
      error: null,
      errorInfo: null,
    }
  }

  static getDerivedStateFromError(error: Error): State {
    return {
      hasError: true,
      error,
      errorInfo: null,
    }
  }

  componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    // Log error to monitoring service (Sentry, etc.)
    console.error('ErrorBoundary caught an error:', error, errorInfo)

    // Call optional error callback
    this.props.onError?.(error, errorInfo)

    this.setState({
      error,
      errorInfo,
    })

    // Send to error tracking service
    if (typeof window !== 'undefined' && (window as any).Sentry) {
      (window as any).Sentry.captureException(error, {
        contexts: {
          react: {
            componentStack: errorInfo.componentStack,
          },
        },
      })
    }
  }

  handleReset = () => {
    this.setState({
      hasError: false,
      error: null,
      errorInfo: null,
    })
  }

  render() {
    if (this.state.hasError) {
      if (this.props.fallback) {
        return this.props.fallback
      }

      return (
        <div className="min-h-screen flex items-center justify-center p-4 bg-background">
          <Card className="max-w-2xl w-full">
            <CardHeader>
              <div className="flex items-center gap-3">
                <div className="p-2 rounded-full bg-destructive/10">
                  <AlertTriangle className="h-6 w-6 text-destructive" />
                </div>
                <div>
                  <CardTitle>Something went wrong</CardTitle>
                  <CardDescription>
                    An unexpected error occurred. We've been notified and are working to fix it.
                  </CardDescription>
                </div>
              </div>
            </CardHeader>
            <CardContent className="space-y-4">
              {process.env.NODE_ENV === 'development' && this.state.error && (
                <div className="rounded-lg bg-muted p-4">
                  <h3 className="font-semibold mb-2 text-sm">Error Details (Development Only):</h3>
                  <div className="space-y-2">
                    <div>
                      <span className="text-xs font-mono text-destructive">
                        {this.state.error.name}: {this.state.error.message}
                      </span>
                    </div>
                    {this.state.errorInfo && (
                      <details className="text-xs">
                        <summary className="cursor-pointer font-semibold mb-1">
                          Component Stack
                        </summary>
                        <pre className="mt-2 overflow-auto max-h-48 bg-background p-2 rounded border">
                          {this.state.errorInfo.componentStack}
                        </pre>
                      </details>
                    )}
                    {this.state.error.stack && (
                      <details className="text-xs">
                        <summary className="cursor-pointer font-semibold mb-1">
                          Stack Trace
                        </summary>
                        <pre className="mt-2 overflow-auto max-h-48 bg-background p-2 rounded border">
                          {this.state.error.stack}
                        </pre>
                      </details>
                    )}
                  </div>
                </div>
              )}

              <div className="flex gap-2">
                <Button onClick={this.handleReset} variant="default">
                  <RefreshCw className="h-4 w-4 mr-2" />
                  Try Again
                </Button>
                <Button onClick={() => (window.location.href = '/')} variant="outline">
                  <Home className="h-4 w-4 mr-2" />
                  Go Home
                </Button>
              </div>

              <p className="text-sm text-muted-foreground">
                If this problem persists, please contact support with the error details above.
              </p>
            </CardContent>
          </Card>
        </div>
      )
    }

    return this.props.children
  }
}

// Hook version for functional components
export function useErrorHandler() {
  const [error, setError] = React.useState<Error | null>(null)

  React.useEffect(() => {
    if (error) {
      throw error
    }
  }, [error])

  return setError
}
