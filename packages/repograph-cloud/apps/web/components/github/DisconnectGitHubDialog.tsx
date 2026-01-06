"use client"

import { AlertTriangle,Loader2 } from "lucide-react"
import { useState } from "react"

import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import { useGitHubConnection } from "@/lib/hooks/use-github-connection"

export function DisconnectGitHubDialog() {
  const { disconnect } = useGitHubConnection()
  const [open, setOpen] = useState(false)
  const [isDisconnecting, setIsDisconnecting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const handleDisconnect = async () => {
    try {
      setIsDisconnecting(true)
      setError(null)
      await disconnect()
      setOpen(false)
    } catch (err: any) {
      setError(err.message || "Failed to disconnect GitHub account")
    } finally {
      setIsDisconnecting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="destructive" size="sm">
          Disconnect
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Disconnect GitHub Account</DialogTitle>
          <DialogDescription>
            Are you sure you want to disconnect your GitHub account?
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4">
          <div className="flex items-start space-x-3 rounded-lg border border-yellow-500/50 bg-yellow-500/10 p-4">
            <AlertTriangle className="h-5 w-5 text-yellow-500 mt-0.5" />
            <div className="space-y-1">
              <p className="text-sm font-medium">Warning</p>
              <p className="text-sm text-muted-foreground">
                Disconnecting your GitHub account will:
              </p>
              <ul className="ml-4 list-disc space-y-1 text-sm text-muted-foreground">
                <li>Remove access to your GitHub repositories</li>
                <li>Stop automatic synchronization</li>
                <li>Revoke all GitHub-related permissions</li>
              </ul>
              <p className="text-sm text-muted-foreground mt-2">
                Your imported repositories will remain but won't be updated.
              </p>
            </div>
          </div>

          {error && (
            <div className="rounded-lg border border-destructive bg-destructive/10 p-3">
              <p className="text-sm text-destructive">{error}</p>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => setOpen(false)}
            disabled={isDisconnecting}
          >
            Cancel
          </Button>
          <Button
            variant="destructive"
            onClick={handleDisconnect}
            disabled={isDisconnecting}
          >
            {isDisconnecting && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Disconnect
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
