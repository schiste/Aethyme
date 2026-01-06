import axios from 'axios'

const apiClient = axios.create({
  baseURL: process.env.NEXT_PUBLIC_API_URL || 'http://localhost:8000/api/v1',
  headers: {
    'Content-Type': 'application/json',
  },
})

// Add auth interceptor
apiClient.interceptors.request.use((config) => {
  if (typeof window !== 'undefined') {
    const token = localStorage.getItem('access_token')
    if (token) {
      config.headers.Authorization = `Bearer ${token}`
    }
  }
  return config
})

// Add response interceptor for token refresh
apiClient.interceptors.response.use(
  (response) => response,
  async (error) => {
    const originalRequest = error.config

    // If 401 and we have a refresh token, try to refresh
    if (error.response?.status === 401 && !originalRequest._retry) {
      originalRequest._retry = true

      if (typeof window !== 'undefined') {
        const refreshToken = localStorage.getItem('refresh_token')
        if (refreshToken) {
          try {
            const { data } = await axios.post(
              `${process.env.NEXT_PUBLIC_API_URL || 'http://localhost:8000/api/v1'}/auth/refresh`,
              { refresh_token: refreshToken }
            )
            localStorage.setItem('access_token', data.access_token)
            originalRequest.headers.Authorization = `Bearer ${data.access_token}`
            return apiClient(originalRequest)
          } catch (refreshError) {
            // Refresh failed, clear tokens and redirect to login
            localStorage.removeItem('access_token')
            localStorage.removeItem('refresh_token')
            window.location.href = '/auth/login'
            return Promise.reject(refreshError)
          }
        }
      }
    }

    return Promise.reject(error)
  }
)

export default apiClient
