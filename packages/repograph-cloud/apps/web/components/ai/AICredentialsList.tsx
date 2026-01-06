'use client'

import { formatDistanceToNow } from 'date-fns'
import { CheckCircle2, MoreHorizontal, RefreshCw, Trash2,XCircle } from 'lucide-react'
import { useState } from 'react'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useAICredentials } from '@/lib/hooks/useAICredentials'

import { DeleteCredentialDialog } from './DeleteCredentialDialog'

interface AICredential {
  id: number
  provider_name: string
  provider_type: string
  is_active: boolean
  is_validated: boolean
  last_validated_at: string | null
  total_requests: number
  total_tokens_used: number
  created_at: string
}

export function AICredentialsList() {
  const { credentials, loading, error, revalidateCredential, deleteCredential } = useAICredentials()
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false)
  const [selectedCredential, setSelectedCredential] = useState<AICredential | null>(null)
  const [revalidating, setRevalidating] = useState<number | null>(null)

  const handleRevalidate = async (credentialId: number) => {
    setRevalidating(credentialId)
    try {
      await revalidateCredential(credentialId)
    } finally {
      setRevalidating(null)
    }
  }

  const handleDelete = (credential: AICredential) => {
    setSelectedCredential(credential)
    setDeleteDialogOpen(true)
  }

  const confirmDelete = async () => {
    if (selectedCredential) {
      await deleteCredential(selectedCredential.id)
      setDeleteDialogOpen(false)
      setSelectedCredential(null)
    }
  }

  const getProviderBadge = (type: string) => {
    const colors: Record<string, string> = {
      openai: 'bg-green-500/10 text-green-700 dark:text-green-400',
      claude: 'bg-purple-500/10 text-purple-700 dark:text-purple-400',
      azure_openai: 'bg-blue-500/10 text-blue-700 dark:text-blue-400',
    }
    return colors[type] || 'bg-gray-500/10 text-gray-700 dark:text-gray-400'
  }

  const getProviderLabel = (type: string) => {
    const labels: Record<string, string> = {
      openai: 'OpenAI',
      claude: 'Claude',
      azure_openai: 'Azure OpenAI',
    }
    return labels[type] || type
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center py-8">
        <RefreshCw className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (error) {
    return (
      <div className="text-center py-8">
        <p className="text-sm text-destructive">{error}</p>
      </div>
    )
  }

  if (!credentials || credentials.length === 0) {
    return (
      <div className="text-center py-8">
        <p className="text-sm text-muted-foreground">
          No AI credentials configured yet. Add your first API key to enable semantic search.
        </p>
      </div>
    )
  }

  return (
    <>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Provider</TableHead>
            <TableHead>Status</TableHead>
            <TableHead>Usage</TableHead>
            <TableHead>Last Validated</TableHead>
            <TableHead className="text-right">Actions</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {credentials.map((credential) => (
            <TableRow key={credential.id}>
              <TableCell className="font-medium">{credential.provider_name}</TableCell>
              <TableCell>
                <Badge className={getProviderBadge(credential.provider_type)} variant="secondary">
                  {getProviderLabel(credential.provider_type)}
                </Badge>
              </TableCell>
              <TableCell>
                <div className="flex items-center gap-2">
                  {credential.is_validated ? (
                    <CheckCircle2 className="h-4 w-4 text-green-600" />
                  ) : (
                    <XCircle className="h-4 w-4 text-red-600" />
                  )}
                  <span className="text-sm">
                    {credential.is_validated ? 'Valid' : 'Invalid'}
                  </span>
                </div>
              </TableCell>
              <TableCell>
                <div className="text-sm">
                  <div>{credential.total_requests.toLocaleString()} requests</div>
                  <div className="text-muted-foreground">
                    {credential.total_tokens_used.toLocaleString()} tokens
                  </div>
                </div>
              </TableCell>
              <TableCell>
                <span className="text-sm text-muted-foreground">
                  {credential.last_validated_at
                    ? formatDistanceToNow(new Date(credential.last_validated_at), {
                        addSuffix: true,
                      })
                    : 'Never'}
                </span>
              </TableCell>
              <TableCell className="text-right">
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button variant="ghost" size="sm">
                      <MoreHorizontal className="h-4 w-4" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem
                      onClick={() => handleRevalidate(credential.id)}
                      disabled={revalidating === credential.id}
                    >
                      <RefreshCw
                        className={`h-4 w-4 mr-2 ${
                          revalidating === credential.id ? 'animate-spin' : ''
                        }`}
                      />
                      Revalidate
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      onClick={() => handleDelete(credential)}
                      className="text-destructive"
                    >
                      <Trash2 className="h-4 w-4 mr-2" />
                      Delete
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>

      <DeleteCredentialDialog
        open={deleteDialogOpen}
        onOpenChange={setDeleteDialogOpen}
        onConfirm={confirmDelete}
        credentialName={selectedCredential?.provider_name || ''}
      />
    </>
  )
}
