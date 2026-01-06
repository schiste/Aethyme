import { useMutation } from '@tanstack/react-query'
import { useRouter } from 'next/navigation'

import apiClient from '@/lib/api-client'
import { type LoginInput, type RegisterInput } from '@/lib/validators/auth'

interface AuthResponse {
  access_token: string
  refresh_token: string
  token_type: string
  user: {
    id: string
    email: string
    full_name: string
  }
}

export function useLogin() {
  const router = useRouter()

  return useMutation({
    mutationFn: async (credentials: LoginInput) => {
      const { data } = await apiClient.post<AuthResponse>('/auth/login', credentials)
      return data
    },
    onSuccess: (data) => {
      localStorage.setItem('access_token', data.access_token)
      localStorage.setItem('refresh_token', data.refresh_token)
      router.push('/dashboard')
    },
  })
}

export function useRegister() {
  const router = useRouter()

  return useMutation({
    mutationFn: async (userData: Omit<RegisterInput, 'confirmPassword'>) => {
      const { data } = await apiClient.post<AuthResponse>('/auth/register', userData)
      return data
    },
    onSuccess: (data) => {
      localStorage.setItem('access_token', data.access_token)
      localStorage.setItem('refresh_token', data.refresh_token)
      router.push('/dashboard')
    },
  })
}

export function useLogout() {
  const router = useRouter()

  return () => {
    localStorage.removeItem('access_token')
    localStorage.removeItem('refresh_token')
    router.push('/auth/login')
  }
}
