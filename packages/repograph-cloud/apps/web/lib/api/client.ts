import axios, { type AxiosError, type AxiosInstance, type AxiosRequestConfig, type AxiosResponse } from 'axios'

const MAX_RETRIES = 3
const RETRY_DELAY = 1000 // 1 second
const TIMEOUT = 30000 // 30 seconds

interface RetryConfig extends AxiosRequestConfig {
  _retry?: number
  _retryDelay?: number
}

class APIClient {
  private client: AxiosInstance
  private refreshTokenPromise: Promise<string> | null = null

  constructor(baseURL: string = process.env.NEXT_PUBLIC_API_URL || 'http://localhost:8000') {
    this.client = axios.create({
      baseURL,
      timeout: TIMEOUT,
      headers: {
        'Content-Type': 'application/json',
      },
    })

    this.setupInterceptors()
  }

  private setupInterceptors() {
    // Request interceptor - Add auth token
    this.client.interceptors.request.use(
      (config) => {
        const token = this.getToken()
        if (token) {
          config.headers.Authorization = `Bearer ${token}`
        }
        return config
      },
      (error) => {
        return Promise.reject(error)
      }
    )

    // Response interceptor - Handle errors and retries
    this.client.interceptors.response.use(
      (response) => response,
      async (error: AxiosError) => {
        const originalRequest = error.config as RetryConfig

        // Handle 401 Unauthorized - Try to refresh token
        if (error.response?.status === 401 && originalRequest && !originalRequest._retry) {
          originalRequest._retry = true

          try {
            const newToken = await this.refreshToken()
            if (newToken) {
              originalRequest.headers = originalRequest.headers || {}
              originalRequest.headers.Authorization = `Bearer ${newToken}`
              return this.client(originalRequest)
            }
          } catch (refreshError) {
            // Refresh failed, redirect to login
            this.handleAuthFailure()
            return Promise.reject(refreshError)
          }
        }

        // Handle network errors and 5xx errors - Retry with exponential backoff
        if (this.shouldRetry(error) && originalRequest) {
          const retryCount = originalRequest._retry || 0

          if (retryCount < MAX_RETRIES) {
            originalRequest._retry = retryCount + 1
            const delay = RETRY_DELAY * Math.pow(2, retryCount) // Exponential backoff

            console.warn(`Retrying request (${retryCount + 1}/${MAX_RETRIES})...`, {
              url: originalRequest.url,
              delay,
            })

            await this.sleep(delay)
            return this.client(originalRequest)
          }
        }

        // Log error for monitoring
        this.logError(error)

        return Promise.reject(this.normalizeError(error))
      }
    )
  }

  private shouldRetry(error: AxiosError): boolean {
    // Retry on network errors
    if (!error.response) {
      return true
    }

    // Retry on 5xx server errors
    const status = error.response.status
    if (status >= 500 && status < 600) {
      return true
    }

    // Retry on specific errors
    if (status === 429) { // Too Many Requests
      return true
    }

    return false
  }

  private async refreshToken(): Promise<string | null> {
    // Prevent multiple simultaneous refresh attempts
    if (this.refreshTokenPromise) {
      return this.refreshTokenPromise
    }

    this.refreshTokenPromise = (async () => {
      try {
        const refreshToken = this.getRefreshToken()
        if (!refreshToken) {
          throw new Error('No refresh token available')
        }

        const response = await axios.post(
          `${this.client.defaults.baseURL}/api/v1/auth/refresh`,
          { refresh_token: refreshToken }
        )

        const { access_token } = response.data
        this.setToken(access_token)
        return access_token
      } catch (error) {
        this.clearAuth()
        throw error
      } finally {
        this.refreshTokenPromise = null
      }
    })()

    return this.refreshTokenPromise
  }

  private handleAuthFailure() {
    this.clearAuth()
    if (typeof window !== 'undefined') {
      // Redirect to login
      window.location.href = '/login'
    }
  }

  private normalizeError(error: AxiosError): Error {
    if (error.response?.data) {
      const data = error.response.data as any
      return new Error(data.message || data.detail || 'An error occurred')
    }

    if (error.message) {
      return new Error(error.message)
    }

    return new Error('An unexpected error occurred')
  }

  private logError(error: AxiosError) {
    // Log to console in development
    if (process.env.NODE_ENV === 'development') {
      console.error('API Error:', {
        url: error.config?.url,
        method: error.config?.method,
        status: error.response?.status,
        message: error.message,
        data: error.response?.data,
      })
    }

    // Send to error tracking service (Sentry, etc.)
    if (typeof window !== 'undefined' && (window as any).Sentry) {
      (window as any).Sentry.captureException(error, {
        contexts: {
          api: {
            url: error.config?.url,
            method: error.config?.method,
            status: error.response?.status,
          },
        },
      })
    }
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms))
  }

  // Token management
  private getToken(): string | null {
    if (typeof window === 'undefined') return null
    return localStorage.getItem('access_token')
  }

  private getRefreshToken(): string | null {
    if (typeof window === 'undefined') return null
    return localStorage.getItem('refresh_token')
  }

  private setToken(token: string) {
    if (typeof window === 'undefined') return
    localStorage.setItem('access_token', token)
  }

  private clearAuth() {
    if (typeof window === 'undefined') return
    localStorage.removeItem('access_token')
    localStorage.removeItem('refresh_token')
  }

  // Public methods
  async get<T = any>(url: string, config?: AxiosRequestConfig): Promise<AxiosResponse<T>> {
    return this.client.get<T>(url, config)
  }

  async post<T = any>(url: string, data?: any, config?: AxiosRequestConfig): Promise<AxiosResponse<T>> {
    return this.client.post<T>(url, data, config)
  }

  async put<T = any>(url: string, data?: any, config?: AxiosRequestConfig): Promise<AxiosResponse<T>> {
    return this.client.put<T>(url, data, config)
  }

  async patch<T = any>(url: string, data?: any, config?: AxiosRequestConfig): Promise<AxiosResponse<T>> {
    return this.client.patch<T>(url, data, config)
  }

  async delete<T = any>(url: string, config?: AxiosRequestConfig): Promise<AxiosResponse<T>> {
    return this.client.delete<T>(url, config)
  }
}

// Export singleton instance
export const apiClient = new APIClient()
