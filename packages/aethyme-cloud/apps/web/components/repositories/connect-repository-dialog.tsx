'use client'

import { zodResolver } from '@hookform/resolvers/zod'
import { ChevronDown, Plus } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { z } from 'zod'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useCreateRepository } from '@/lib/hooks/use-repositories'

const repositorySchema = z.object({
  name: z.string().min(1, 'Repository name is required'),
  full_name: z.string().min(1, 'Full name is required (e.g., owner/repo)'),
  url: z.string().url('Must be a valid URL'),
  provider: z.enum(['github', 'gitlab', 'bitbucket']),
  provider_id: z.string().min(1, 'Provider ID is required'),
  is_private: z.boolean().optional(),
})

type RepositoryFormData = z.infer<typeof repositorySchema>

export function ConnectRepositoryDialog() {
  const [open, setOpen] = useState(false)
  const [provider, setProvider] = useState<'github' | 'gitlab' | 'bitbucket'>('github')
  const createRepo = useCreateRepository()

  const {
    register,
    handleSubmit,
    formState: { errors },
    reset,
  } = useForm<RepositoryFormData>({
    resolver: zodResolver(repositorySchema),
    defaultValues: {
      provider: 'github',
    },
  })

  const onSubmit = (data: RepositoryFormData) => {
    createRepo.mutate(
      { ...data, provider },
      {
        onSuccess: () => {
          setOpen(false)
          reset()
        },
      }
    )
  }

  const providerLabels = {
    github: 'GitHub',
    gitlab: 'GitLab',
    bitbucket: 'Bitbucket',
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button>
          <Plus className="mr-2 h-4 w-4" />
          Connect Repository
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-[500px]">
        <DialogHeader>
          <DialogTitle>Connect Repository</DialogTitle>
          <DialogDescription>
            Add a code repository to start indexing and querying
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit(onSubmit)}>
          <div className="space-y-4 py-4">
            {createRepo.isError && (
              <div className="p-3 text-sm text-red-500 bg-red-50 dark:bg-red-900/10 border border-red-200 dark:border-red-800 rounded-md">
                {(createRepo.error as any)?.response?.data?.detail || 'Failed to connect repository'}
              </div>
            )}

            <div className="space-y-2">
              <Label>Provider</Label>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline" className="w-full justify-between">
                    {providerLabels[provider]}
                    <ChevronDown className="h-4 w-4 opacity-50" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent className="w-full">
                  <DropdownMenuItem onClick={() => setProvider('github')}>
                    GitHub
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => setProvider('gitlab')}>
                    GitLab
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => setProvider('bitbucket')}>
                    Bitbucket
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
              <input type="hidden" value={provider} {...register('provider')} />
            </div>

            <div className="space-y-2">
              <Label htmlFor="name">Repository Name</Label>
              <Input
                id="name"
                placeholder="my-awesome-project"
                {...register('name')}
                disabled={createRepo.isPending}
              />
              {errors.name && (
                <p className="text-sm text-red-500">{errors.name.message}</p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="full_name">Full Name</Label>
              <Input
                id="full_name"
                placeholder="username/my-awesome-project"
                {...register('full_name')}
                disabled={createRepo.isPending}
              />
              {errors.full_name && (
                <p className="text-sm text-red-500">{errors.full_name.message}</p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="url">Repository URL</Label>
              <Input
                id="url"
                type="url"
                placeholder="https://github.com/username/repo"
                {...register('url')}
                disabled={createRepo.isPending}
              />
              {errors.url && (
                <p className="text-sm text-red-500">{errors.url.message}</p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="provider_id">Provider ID</Label>
              <Input
                id="provider_id"
                placeholder="12345678"
                {...register('provider_id')}
                disabled={createRepo.isPending}
              />
              <p className="text-xs text-muted-foreground">
                The numeric ID from {providerLabels[provider]}
              </p>
              {errors.provider_id && (
                <p className="text-sm text-red-500">{errors.provider_id.message}</p>
              )}
            </div>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setOpen(false)}
              disabled={createRepo.isPending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={createRepo.isPending}>
              {createRepo.isPending ? 'Connecting...' : 'Connect'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
