'use client'

import { zodResolver } from '@hookform/resolvers/zod'
import { AlertTriangle,Check, Copy, Plus } from 'lucide-react'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useCreateAPIKey } from '@/lib/hooks/use-api-keys'

const apiKeySchema = z.object({
  name: z.string().min(1, 'Name is required'),
  expires_in_days: z.number().min(1).max(365).optional(),
})

type APIKeyFormData = z.infer<typeof apiKeySchema>

export function CreateAPIKeyDialog() {
  const [open, setOpen] = useState(false)
  const [createdKey, setCreatedKey] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  const createKey = useCreateAPIKey()

  const {
    register,
    handleSubmit,
    formState: { errors },
    reset,
  } = useForm<APIKeyFormData>({
    resolver: zodResolver(apiKeySchema),
  })

  const onSubmit = (data: APIKeyFormData) => {
    createKey.mutate(data, {
      onSuccess: (response) => {
        setCreatedKey(response.key)
        reset()
      },
    })
  }

  const handleCopy = async () => {
    if (createdKey) {
      await navigator.clipboard.writeText(createdKey)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    }
  }

  const handleClose = () => {
    setOpen(false)
    setCreatedKey(null)
    setCopied(false)
    reset()
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button>
          <Plus className="mr-2 h-4 w-4" />
          Create API Key
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-[500px]">
        <DialogHeader>
          <DialogTitle>
            {createdKey ? 'API Key Created' : 'Create API Key'}
          </DialogTitle>
          <DialogDescription>
            {createdKey
              ? 'Save this key now - you won\'t be able to see it again!'
              : 'Generate a new API key for programmatic access'}
          </DialogDescription>
        </DialogHeader>

        {createdKey ? (
          <div className="space-y-4 py-4">
            <div className="p-4 bg-yellow-50 dark:bg-yellow-900/10 border border-yellow-200 dark:border-yellow-800 rounded-md">
              <div className="flex gap-2">
                <AlertTriangle className="h-5 w-5 text-yellow-600 dark:text-yellow-400 flex-shrink-0 mt-0.5" />
                <div className="text-sm text-yellow-800 dark:text-yellow-200">
                  <p className="font-semibold">Important: Save this key now!</p>
                  <p className="mt-1">
                    This is the only time you&apos;ll see the full key. Store it somewhere safe.
                  </p>
                </div>
              </div>
            </div>

            <div className="space-y-2">
              <Label>Your API Key</Label>
              <div className="flex gap-2">
                <Input
                  value={createdKey}
                  readOnly
                  className="font-mono text-sm"
                />
                <Button
                  type="button"
                  variant="outline"
                  size="icon"
                  onClick={handleCopy}
                >
                  {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
                </Button>
              </div>
            </div>

            <div className="p-3 bg-slate-50 dark:bg-slate-800 rounded-md">
              <p className="text-sm text-muted-foreground">
                Use this key in your API requests by adding it to the Authorization header:
              </p>
              <code className="text-xs block mt-2 p-2 bg-slate-100 dark:bg-slate-900 rounded">
                Authorization: Bearer {createdKey}
              </code>
            </div>
          </div>
        ) : (
          <form onSubmit={handleSubmit(onSubmit)}>
            <div className="space-y-4 py-4">
              {createKey.isError && (
                <div className="p-3 text-sm text-red-500 bg-red-50 dark:bg-red-900/10 border border-red-200 dark:border-red-800 rounded-md">
                  {(createKey.error as any)?.response?.data?.detail || 'Failed to create API key'}
                </div>
              )}

              <div className="space-y-2">
                <Label htmlFor="name">Name</Label>
                <Input
                  id="name"
                  placeholder="Production API Key"
                  {...register('name')}
                  disabled={createKey.isPending}
                />
                {errors.name && (
                  <p className="text-sm text-red-500">{errors.name.message}</p>
                )}
              </div>

              <div className="space-y-2">
                <Label htmlFor="expires_in_days">Expires in (days)</Label>
                <Input
                  id="expires_in_days"
                  type="number"
                  placeholder="30"
                  {...register('expires_in_days', { valueAsNumber: true })}
                  disabled={createKey.isPending}
                />
                <p className="text-xs text-muted-foreground">
                  Leave empty for no expiration (1-365 days)
                </p>
                {errors.expires_in_days && (
                  <p className="text-sm text-red-500">{errors.expires_in_days.message}</p>
                )}
              </div>
            </div>

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={handleClose}
                disabled={createKey.isPending}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={createKey.isPending}>
                {createKey.isPending ? 'Creating...' : 'Create Key'}
              </Button>
            </DialogFooter>
          </form>
        )}

        {createdKey && (
          <DialogFooter>
            <Button onClick={handleClose} className="w-full">
              I&apos;ve saved my key
            </Button>
          </DialogFooter>
        )}
      </DialogContent>
    </Dialog>
  )
}
