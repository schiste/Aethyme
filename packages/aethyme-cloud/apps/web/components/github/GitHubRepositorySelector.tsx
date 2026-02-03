"use client"

import { Avatar, Input } from '@aeptus/ui'
import {
  AlertCircle,
  Calendar,
  FileCode,
  GitFork,
  Globe,
  Loader2,
  Lock,
  Search,
  Star,
} from "lucide-react"
import { useMemo,useState } from "react"

import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { jobsApi } from "@/lib/api/jobs"
import { repositoriesApi } from "@/lib/api/repositories"
import { useGitHubConnection } from "@/lib/hooks/use-github-connection"
import { useGitHubRepositories } from "@/lib/hooks/use-github-repositories"
import type { GitHubRepository } from "@/types/github"

import { ImportProgressTracker } from "../jobs/ImportProgressTracker"

export function GitHubRepositorySelector() {
  const { isConnected } = useGitHubConnection()
  const { repositories, isLoading, error } = useGitHubRepositories()
  const [searchQuery, setSearchQuery] = useState("")
  const [selectedRepo, setSelectedRepo] = useState<GitHubRepository | null>(null)
  const [importingJobId, setImportingJobId] = useState<string | null>(null)
  const [importError, setImportError] = useState<string | null>(null)

  const filteredRepositories = useMemo(() => {
    if (!searchQuery) return repositories

    const query = searchQuery.toLowerCase()
    return repositories.filter(
      (repo) =>
        repo.name.toLowerCase().includes(query) ||
        repo.full_name.toLowerCase().includes(query) ||
        repo.description?.toLowerCase().includes(query)
    )
  }, [repositories, searchQuery])

  const handleImport = async (repo: GitHubRepository) => {
    try {
      setImportError(null)
      setSelectedRepo(repo)

      // Step 1: Create repository in backend
      const createdRepo = await repositoriesApi.create({
        name: repo.name,
        url: repo.html_url,
        github_id: repo.id.toString(),
        description: repo.description,
        language: repo.language,
        stars: repo.stargazers_count,
        forks: repo.forks_count,
        is_private: repo.private,
        default_branch: "main", // Will be updated during indexing
      })

      // Step 2: Trigger indexing job with the created repository ID
      const response = await jobsApi.indexRepository({
        repository_id: createdRepo.id,
      })

      setImportingJobId(response.job_id)
    } catch (err: any) {
      const errorMsg = err.response?.data?.detail || err.message || "Failed to start import"
      setImportError(errorMsg)
    }
  }

  if (!isConnected) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Import Repositories</CardTitle>
          <CardDescription>
            Connect your GitHub account to import repositories
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <AlertCircle className="h-12 w-12 text-muted-foreground mb-4" />
            <p className="text-sm text-muted-foreground">
              Please connect your GitHub account first to browse and import repositories.
            </p>
          </div>
        </CardContent>
      </Card>
    )
  }

  if (importingJobId && selectedRepo) {
    return (
      <ImportProgressTracker
        jobId={importingJobId}
        repositoryName={selectedRepo.full_name}
        onClose={() => {
          setImportingJobId(null)
          setSelectedRepo(null)
        }}
      />
    )
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Import Repositories</CardTitle>
        <CardDescription>
          Browse and import your GitHub repositories
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="relative">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder="Search repositories..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="pl-10"
          />
        </div>

        {importError && (
          <div className="rounded-lg border border-destructive bg-destructive/10 p-3">
            <p className="text-sm text-destructive">{importError}</p>
          </div>
        )}

        {isLoading && (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
          </div>
        )}

        {error && (
          <div className="rounded-lg border border-destructive bg-destructive/10 p-4">
            <p className="text-sm text-destructive">
              Failed to load repositories: {error.message}
            </p>
          </div>
        )}

        {!isLoading && !error && filteredRepositories.length === 0 && (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <FileCode className="h-12 w-12 text-muted-foreground mb-4" />
            <p className="text-sm text-muted-foreground">
              {searchQuery
                ? "No repositories found matching your search"
                : "No repositories found"}
            </p>
          </div>
        )}

        {!isLoading && !error && filteredRepositories.length > 0 && (
          <div className="space-y-3">
            <p className="text-sm text-muted-foreground">
              Found {filteredRepositories.length} repositories
            </p>

            <div className="space-y-2 max-h-[600px] overflow-y-auto">
              {filteredRepositories.map((repo) => (
                <RepositoryCard
                  key={repo.id}
                  repository={repo}
                  onImport={handleImport}
                />
              ))}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

interface RepositoryCardProps {
  repository: GitHubRepository
  onImport: (repo: GitHubRepository) => void
}

function RepositoryCard({ repository, onImport }: RepositoryCardProps) {
  const [isImporting, setIsImporting] = useState(false)

  const handleClick = async () => {
    setIsImporting(true)
    await onImport(repository)
    setIsImporting(false)
  }

  return (
    <div className="rounded-lg border p-4 hover:bg-accent transition-colors">
      <div className="flex items-start justify-between">
        <div className="flex items-start space-x-3 flex-1">
          <Avatar className="h-10 w-10">
            <AvatarImage src={repository.owner.avatar_url} />
            <AvatarFallback>
              {repository.owner.login.substring(0, 2).toUpperCase()}
            </AvatarFallback>
          </Avatar>

          <div className="flex-1 space-y-2">
            <div className="flex items-center space-x-2">
              <h4 className="font-semibold text-sm">{repository.full_name}</h4>
              {repository.private ? (
                <Lock className="h-3 w-3 text-muted-foreground" />
              ) : (
                <Globe className="h-3 w-3 text-muted-foreground" />
              )}
            </div>

            {repository.description && (
              <p className="text-sm text-muted-foreground line-clamp-2">
                {repository.description}
              </p>
            )}

            <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
              {repository.language && (
                <div className="flex items-center space-x-1">
                  <span className="h-2 w-2 rounded-full bg-primary" />
                  <span>{repository.language}</span>
                </div>
              )}

              <div className="flex items-center space-x-1">
                <Star className="h-3 w-3" />
                <span>{repository.stargazers_count.toLocaleString()}</span>
              </div>

              <div className="flex items-center space-x-1">
                <GitFork className="h-3 w-3" />
                <span>{repository.forks_count.toLocaleString()}</span>
              </div>

              <div className="flex items-center space-x-1">
                <Calendar className="h-3 w-3" />
                <span>
                  Updated {new Date(repository.updated_at).toLocaleDateString()}
                </span>
              </div>
            </div>
          </div>
        </div>

        <Button
          size="sm"
          onClick={handleClick}
          disabled={isImporting}
        >
          {isImporting && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
          Import
        </Button>
      </div>
    </div>
  )
}
