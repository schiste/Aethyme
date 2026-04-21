'use client'

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'

interface DeleteCredentialDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onConfirm: () => void
  credentialName: string
}

export function DeleteCredentialDialog({
  open,
  onOpenChange,
  onConfirm,
  credentialName,
}: DeleteCredentialDialogProps) {
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete AI Credential?</AlertDialogTitle>
          <AlertDialogDescription>
            Are you sure you want to delete &quot;{credentialName}&quot;? This will prevent semantic search
            from working until you add a new credential. This action cannot be undone.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={onConfirm} className="bg-destructive text-destructive-foreground hover:bg-destructive/90">
            Delete
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
