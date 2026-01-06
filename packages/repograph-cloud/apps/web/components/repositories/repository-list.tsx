'use client'

import { formatDistanceToNow } from 'date-fns'
import { ExternalLink,RefreshCw, Trash2 } from 'lucide-react'

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
import { useDeleteRepository, useReindexRepository,useRepositories } from '@/lib/hooks/use-repositories'

export function RepositoryList() {
  const { data, isLoading, error } = useRepositories()
  const deleteRepo = useDeleteRepository()
  const reindexRepo = useReindexRepository()

  if (isLoading) {
    return (
      <div className="text-center py-12">
        <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto"></div>
        <p className="mt-4 text-sm text-muted-foreground">Loading repositories...</p>
      </div>
    )
  }

  if (error) {
    return (
      <div className="text-center py-12">
        <p className="text-red-500">Failed to load repositories</p>
      </div>
    )
  }

  if (!data?.items.length) {
    return (
      <div className="text-center py-12">
        <p className="text-muted-foreground">No repositories yet</p>
        <p className="text-sm text-muted-foreground mt-2">
          Connect your first repository to get started
        </p>
      </div>
    )
  }

  const getStatusBadge = (status: string) => {
    const variants: Record<string, 'default' | 'secondary' | 'outline' | 'destructive'> = {
      pending: 'secondary',
      indexing: 'default',
      completed: 'outline',
      failed: 'destructive',
    }
    return (
      <Badge variant={variants[status] || 'secondary'}>
        {status}
      </Badge>
    )
  }

  return (
    <div className="rounded-md border">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Repository</TableHead>
            <TableHead>Provider</TableHead>
            <TableHead>Status</TableHead>
            <TableHead>Created</TableHead>
            <TableHead className="text-right">Actions</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {data.items.map((repo) => (
            <TableRow key={repo.id}>
              <TableCell>
                <div>
                  <div className="font-medium">{repo.name}</div>
                  <div className="text-sm text-muted-foreground">{repo.full_name}</div>
                </div>
              </TableCell>
              <TableCell>
                <Badge variant="outline">{repo.provider}</Badge>
              </TableCell>
              <TableCell>{getStatusBadge(repo.indexing_status)}</TableCell>
              <TableCell className="text-sm text-muted-foreground">
                {formatDistanceToNow(new Date(repo.created_at), { addSuffix: true })}
              </TableCell>
              <TableCell className="text-right">
                <div className="flex justify-end gap-2">
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => window.open(repo.url, '_blank')}
                  >
                    <ExternalLink className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => reindexRepo.mutate(repo.id)}
                    disabled={reindexRepo.isPending}
                  >
                    <RefreshCw className={`h-4 w-4 ${reindexRepo.isPending ? 'animate-spin' : ''}`} />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => deleteRepo.mutate(repo.id)}
                    disabled={deleteRepo.isPending}
                  >
                    <Trash2 className="h-4 w-4 text-red-500" />
                  </Button>
                </div>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}
