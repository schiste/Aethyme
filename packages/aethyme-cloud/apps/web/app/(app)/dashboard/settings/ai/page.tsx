'use client'

import { Plus } from 'lucide-react'
import { useState } from 'react'

import { AddAICredentialDialog } from '@/components/ai/AddAICredentialDialog'
import { AICredentialsList } from '@/components/ai/AICredentialsList'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

export default function AISettingsPage() {
  const [showAddDialog, setShowAddDialog] = useState(false)

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">AI Settings</h1>
          <p className="text-muted-foreground mt-2">
            Manage your AI provider credentials for semantic search
          </p>
        </div>
        <Button onClick={() => setShowAddDialog(true)}>
          <Plus className="h-4 w-4 mr-2" />
          Add Credential
        </Button>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>AI Provider Credentials</CardTitle>
          <CardDescription>
            Add your own API keys from OpenAI, Claude, or Azure OpenAI for semantic code search.
            Your keys are encrypted and never shared.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <AICredentialsList />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>How It Works</CardTitle>
          <CardDescription>
            Bring Your Own Key (BYOK) architecture
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div>
            <h4 className="font-semibold mb-2">1. Add Your API Key</h4>
            <p className="text-sm text-muted-foreground">
              Get an API key from OpenAI, Anthropic (Claude), or Azure OpenAI.
              We encrypt and securely store your key.
            </p>
          </div>
          <div>
            <h4 className="font-semibold mb-2">2. Generate Embeddings</h4>
            <p className="text-sm text-muted-foreground">
              We use your API key to generate vector embeddings for your code.
              This happens automatically when you connect a repository.
            </p>
          </div>
          <div>
            <h4 className="font-semibold mb-2">3. Semantic Search</h4>
            <p className="text-sm text-muted-foreground">
              Use natural language queries to find code across your repositories.
              Typical cost: $1-5/month depending on codebase size.
            </p>
          </div>
        </CardContent>
      </Card>

      <AddAICredentialDialog
        open={showAddDialog}
        onOpenChange={setShowAddDialog}
      />
    </div>
  )
}
