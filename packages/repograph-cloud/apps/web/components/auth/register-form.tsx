'use client'

import { Input } from '@aeptus/ui'
import { zodResolver } from '@hookform/resolvers/zod'
import Link from 'next/link'
import { useForm } from 'react-hook-form'

import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { useRegister } from '@/lib/hooks/use-auth'
import { type RegisterInput, registerSchema } from '@/lib/validators/auth'

export function RegisterForm() {
  const register_mutation = useRegister()

  const {
    register,
    handleSubmit,
    formState: { errors },
  } = useForm<RegisterInput>({
    resolver: zodResolver(registerSchema),
  })

  const onSubmit = (data: RegisterInput) => {
    const { confirmPassword, ...registerData } = data
    register_mutation.mutate(registerData)
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Create an account</CardTitle>
        <CardDescription>Get started with RepoGraph Cloud</CardDescription>
      </CardHeader>
      <form onSubmit={handleSubmit(onSubmit)}>
        <CardContent className="space-y-4">
          {register_mutation.isError && (
            <div className="p-3 text-sm text-red-500 bg-red-50 dark:bg-red-900/10 border border-red-200 dark:border-red-800 rounded-md">
              {(register_mutation.error as any)?.response?.data?.detail || 'Registration failed'}
            </div>
          )}

          <div className="space-y-2">
            <Label htmlFor="full_name">Full Name</Label>
            <Input
              id="full_name"
              type="text"
              placeholder="John Doe"
              {...register('full_name')}
              disabled={register_mutation.isPending}
            />
            {errors.full_name && (
              <p className="text-sm text-red-500">{errors.full_name.message}</p>
            )}
          </div>

          <div className="space-y-2">
            <Label htmlFor="email">Email</Label>
            <Input
              id="email"
              type="email"
              placeholder="you@example.com"
              {...register('email')}
              disabled={register_mutation.isPending}
            />
            {errors.email && (
              <p className="text-sm text-red-500">{errors.email.message}</p>
            )}
          </div>

          <div className="space-y-2">
            <Label htmlFor="password">Password</Label>
            <Input
              id="password"
              type="password"
              {...register('password')}
              disabled={register_mutation.isPending}
            />
            {errors.password && (
              <p className="text-sm text-red-500">{errors.password.message}</p>
            )}
          </div>

          <div className="space-y-2">
            <Label htmlFor="confirmPassword">Confirm Password</Label>
            <Input
              id="confirmPassword"
              type="password"
              {...register('confirmPassword')}
              disabled={register_mutation.isPending}
            />
            {errors.confirmPassword && (
              <p className="text-sm text-red-500">{errors.confirmPassword.message}</p>
            )}
          </div>
        </CardContent>

        <CardFooter className="flex flex-col space-y-4">
          <Button type="submit" className="w-full" disabled={register_mutation.isPending}>
            {register_mutation.isPending ? 'Creating account...' : 'Create account'}
          </Button>

          <p className="text-sm text-center text-slate-600 dark:text-slate-400">
            Already have an account?{' '}
            <Link href="/login" className="text-primary hover:underline">
              Sign in
            </Link>
          </p>
        </CardFooter>
      </form>
    </Card>
  )
}
