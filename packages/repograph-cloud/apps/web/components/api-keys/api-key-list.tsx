'use client'

import { formatDistanceToNow } from 'date-fns'
import { Clock,Trash2 } from 'lucide-react'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useAPIKeys, useRevokeAPIKey } from '@/lib/hooks/use-api-keys'

export function APIKeyList() {
  const { data, isLoading, error } = useAPIKeys()
  const revokeKey = useRevokeAPIKey()

  if (isLoading) {
    return (
      <div className="text-center py-12">
        <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto"></div>
        <p className="mt-4 text-sm text-muted-foreground">Loading API keys...</p>
      </div>
    )
  }

  if (error) {
    return (
      <div className="text-center py-12">
        <p className="text-red-500">Failed to load API keys</p>
      </div>
    )
  }

  if (!data?.items.length) {
    return (
      <div className="text-center py-12">
        <p className="text-muted-foreground">No API keys yet</p>
        <p className="text-sm text-muted-foreground mt-2">
          Create your first API key to get started
        </p>
      </div>
    )
  }

  return (
    <div className="rounded-md border">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Key Prefix</TableHead>
            <TableHead>Status</TableHead>
            <TableHead>Last Used</TableHead>
            <TableHead>Expires</TableHead>
            <TableHead className="text-right">Actions</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {data.items.map((key) => (
            <TableRow key={key.id}>
              <TableCell className="font-medium">{key.name}</TableCell>
              <TableCell>
                <code className="text-sm bg-slate-100 dark:bg-slate-800 px-2 py-1 rounded">
                  {key.key_prefix}...
                </code>
              </TableCell>
              <TableCell>
                <Badge variant={key.is_active ? 'outline' : 'destructive'}>
                  {key.is_active ? 'Active' : 'Revoked'}
                </Badge>
              </TableCell>
              <TableCell className="text-sm text-muted-foreground">
                {key.last_used_at
                  ? formatDistanceToNow(new Date(key.last_used_at), { addSuffix: true })
                  : 'Never'}
              </TableCell>
              <TableCell className="text-sm text-muted-foreground">
                {key.expires_at ? (
                  <div className="flex items-center gap-1">
                    <Clock className="h-3 w-3" />
                    {formatDistanceToNow(new Date(key.expires_at), { addSuffix: true })}
                  </div>
                ) : (
                  'Never'
                )}
              </TableCell>
              <TableCell className="text-right">
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => revokeKey.mutate(key.id)}
                  disabled={!key.is_active || revokeKey.isPending}
                >
                  <Trash2 className="h-4 w-4 text-red-500" />
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}
