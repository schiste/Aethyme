'use client'

import { AlertCircle, CheckCircle2,Loader2 } from 'lucide-react'
import { useState } from 'react'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { useAICredentials } from '@/lib/hooks/useAICredentials'

interface AddAICredentialDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function AddAICredentialDialog({ open, onOpenChange }: AddAICredentialDialogProps) {
  const [providerType, setProviderType] = useState<string>('')
  const [providerName, setProviderName] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [azureEndpoint, setAzureEndpoint] = useState('')
  const [azureDeployment, setAzureDeployment] = useState('')
  const [validating, setValidating] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState(false)

  const { addCredential } = useAICredentials()

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(null)
    setValidating(true)
    setSuccess(false)

    try {
      const credentials: any = { api_key: apiKey }

      if (providerType === 'azure_openai') {
        credentials.azure_endpoint = azureEndpoint
        credentials.azure_deployment = azureDeployment
      }

      await addCredential({
        provider_type: providerType,
        provider_name: providerName,
        credentials,
        validate: true,
      })

      setSuccess(true)
      setTimeout(() => {
        resetForm()
        onOpenChange(false)
      }, 1500)
    } catch (err: any) {
      setError(err.message || 'Failed to add credential')
    } finally {
      setValidating(false)
    }
  }

  const resetForm = () => {
    setProviderType('')
    setProviderName('')
    setApiKey('')
    setAzureEndpoint('')
    setAzureDeployment('')
    setError(null)
    setSuccess(false)
  }

  const handleOpenChange = (newOpen: boolean) => {
    if (!newOpen && !validating) {
      resetForm()
    }
    onOpenChange(newOpen)
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-[500px]">
        <form onSubmit={handleSubmit}>
          <DialogHeader>
            <DialogTitle>Add AI Credential</DialogTitle>
            <DialogDescription>
              Add your API key from an AI provider. We&apos;ll validate and encrypt it securely.
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="provider">Provider</Label>
              <Select value={providerType} onValueChange={setProviderType}>
                <SelectTrigger id="provider">
                  <SelectValue placeholder="Select provider" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="openai">OpenAI</SelectItem>
                  <SelectItem value="claude">Claude (Anthropic)</SelectItem>
                  <SelectItem value="azure_openai">Azure OpenAI</SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="name">Name</Label>
              <Input
                id="name"
                placeholder="My OpenAI Key"
                value={providerName}
                onChange={(e) => setProviderName(e.target.value)}
                required
              />
              <p className="text-xs text-muted-foreground">
                A friendly name to identify this credential
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="apiKey">API Key</Label>
              <Input
                id="apiKey"
                type="password"
                placeholder={
                  providerType === 'openai'
                    ? 'sk-...'
                    : providerType === 'claude'
                    ? 'sk-ant-...'
                    : 'Your API key'
                }
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                required
              />
            </div>

            {providerType === 'azure_openai' && (
              <>
                <div className="space-y-2">
                  <Label htmlFor="azureEndpoint">Azure Endpoint</Label>
                  <Input
                    id="azureEndpoint"
                    placeholder="https://your-resource.openai.azure.com"
                    value={azureEndpoint}
                    onChange={(e) => setAzureEndpoint(e.target.value)}
                    required
                  />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="azureDeployment">Deployment Name</Label>
                  <Input
                    id="azureDeployment"
                    placeholder="your-deployment-name"
                    value={azureDeployment}
                    onChange={(e) => setAzureDeployment(e.target.value)}
                    required
                  />
                </div>
              </>
            )}

            {error && (
              <div className="flex items-center gap-2 text-sm text-destructive bg-destructive/10 p-3 rounded-md">
                <AlertCircle className="h-4 w-4" />
                <span>{error}</span>
              </div>
            )}

            {success && (
              <div className="flex items-center gap-2 text-sm text-green-600 dark:text-green-400 bg-green-500/10 p-3 rounded-md">
                <CheckCircle2 className="h-4 w-4" />
                <span>Credential added and validated successfully!</span>
              </div>
            )}
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => handleOpenChange(false)}
              disabled={validating}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={validating || !providerType || success}>
              {validating ? (
                <>
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  Validating...
                </>
              ) : (
                'Add Credential'
              )}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
