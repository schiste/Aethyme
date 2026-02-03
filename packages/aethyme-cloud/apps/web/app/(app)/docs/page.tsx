"use client"

import { BarChart3, BookOpen, Download,FileText, GitBranch } from "lucide-react"
import { useState } from "react"

import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"

export default function DocumentationPage() {
  const [generating, setGenerating] = useState(false)
  const [selectedRepo] = useState("test-repo")

  return (
    <div className="space-y-6 p-6">
      <div>
        <h1 className="text-3xl font-bold">Documentation Generator</h1>
        <p className="text-muted-foreground mt-2">
          Auto-generate comprehensive documentation from your code
        </p>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
        <Card>
          <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
            <CardTitle className="text-sm font-medium">API Docs</CardTitle>
            <FileText className="h-4 w-4 text-muted-foreground" />
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">Generate</div>
            <p className="text-xs text-muted-foreground">
              From function signatures
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
            <CardTitle className="text-sm font-medium">Diagrams</CardTitle>
            <GitBranch className="h-4 w-4 text-muted-foreground" />
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">Visualize</div>
            <p className="text-xs text-muted-foreground">
              Architecture & dependencies
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
            <CardTitle className="text-sm font-medium">Metrics</CardTitle>
            <BarChart3 className="h-4 w-4 text-muted-foreground" />
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">Analyze</div>
            <p className="text-xs text-muted-foreground">
              Code quality metrics
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
            <CardTitle className="text-sm font-medium">README</CardTitle>
            <BookOpen className="h-4 w-4 text-muted-foreground" />
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">Create</div>
            <p className="text-xs text-muted-foreground">
              Auto-generated README
            </p>
          </CardContent>
        </Card>
      </div>

      <Tabs defaultValue="metrics" className="w-full">
        <TabsList className="grid w-full max-w-md grid-cols-4">
          <TabsTrigger value="metrics">
            <BarChart3 className="h-4 w-4 mr-2" />
            Metrics
          </TabsTrigger>
          <TabsTrigger value="diagrams">
            <GitBranch className="h-4 w-4 mr-2" />
            Diagrams
          </TabsTrigger>
          <TabsTrigger value="api">
            <FileText className="h-4 w-4 mr-2" />
            API Docs
          </TabsTrigger>
          <TabsTrigger value="readme">
            <BookOpen className="h-4 w-4 mr-2" />
            README
          </TabsTrigger>
        </TabsList>

        <TabsContent value="metrics" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>Code Metrics Dashboard</CardTitle>
              <CardDescription>
                Comprehensive analysis of code quality and maintainability
              </CardDescription>
            </CardHeader>
            <CardContent>
              <MetricsDashboard repositoryId={selectedRepo} />
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="diagrams" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>Architecture Diagrams</CardTitle>
              <CardDescription>
                Visual representations of your code structure
              </CardDescription>
            </CardHeader>
            <CardContent>
              <DiagramsView repositoryId={selectedRepo} />
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="api" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>API Documentation</CardTitle>
              <CardDescription>
                Generated from function signatures and docstrings
              </CardDescription>
            </CardHeader>
            <CardContent>
              <Button onClick={() => {
                setGenerating(true)
                setTimeout(() => setGenerating(false), 2000)
              }} disabled={generating}>
                {generating ? "Generating..." : "Generate API Docs"}
              </Button>
              <p className="text-sm text-muted-foreground mt-4">
                API documentation will be generated from your code comments and signatures
              </p>
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="readme" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>README Generator</CardTitle>
              <CardDescription>
                Auto-generate comprehensive README.md
              </CardDescription>
            </CardHeader>
            <CardContent>
              <READMEGenerator repositoryId={selectedRepo} />
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  )
}

function MetricsDashboard({ repositoryId }: { repositoryId: string }) {
  const [metrics, setMetrics] = useState<any>(null)
  const [loading, setLoading] = useState(false)

  const loadMetrics = async () => {
    setLoading(true)
    try {
      const response = await fetch(`/api/v1/docs/metrics/${repositoryId}`, {
        headers: {
          Authorization: `Bearer ${localStorage.getItem("access_token")}`,
        },
      })
      const data = await response.json()
      setMetrics(data)
    } catch (error) {
      console.error("Failed to load metrics:", error)
    } finally {
      setLoading(false)
    }
  }

  if (!metrics) {
    return (
      <div className="text-center py-12">
        <Button onClick={loadMetrics} disabled={loading}>
          {loading ? "Loading..." : "Calculate Metrics"}
        </Button>
      </div>
    )
  }

  const summary = metrics.summary || {}
  const complexity = metrics.complexity || {}
  const maintainability = metrics.maintainability || {}

  return (
    <div className="space-y-6">
      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Health Score</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-3xl font-bold">{summary.health_score || 0}/100</div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Avg Complexity</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-3xl font-bold">{complexity.average_complexity || 0}</div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Maintainability</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-3xl font-bold">{maintainability.maintainability_index || 0}/100</div>
          </CardContent>
        </Card>
      </div>

      <div>
        <h3 className="font-semibold mb-2">Complexity Distribution</h3>
        <div className="grid grid-cols-4 gap-2">
          <div className="p-3 border rounded">
            <div className="text-sm text-muted-foreground">Low</div>
            <div className="text-2xl font-bold">{complexity.distribution?.low || 0}</div>
          </div>
          <div className="p-3 border rounded">
            <div className="text-sm text-muted-foreground">Medium</div>
            <div className="text-2xl font-bold">{complexity.distribution?.medium || 0}</div>
          </div>
          <div className="p-3 border rounded">
            <div className="text-sm text-muted-foreground">High</div>
            <div className="text-2xl font-bold text-orange-600">{complexity.distribution?.high || 0}</div>
          </div>
          <div className="p-3 border rounded">
            <div className="text-sm text-muted-foreground">Very High</div>
            <div className="text-2xl font-bold text-red-600">{complexity.distribution?.very_high || 0}</div>
          </div>
        </div>
      </div>
    </div>
  )
}

function DiagramsView({ repositoryId }: { repositoryId: string }) {
  const [diagram, setDiagram] = useState<string | null>(null)
  const [diagramType, setDiagramType] = useState<"component" | "dependencies">("component")
  const [loading, setLoading] = useState(false)

  const generateDiagram = async () => {
    setLoading(true)
    try {
      const endpoint = diagramType === "component"
        ? `/api/v1/docs/diagrams/component/${repositoryId}?format=mermaid`
        : `/api/v1/docs/diagrams/dependencies/${repositoryId}?format=mermaid`

      const response = await fetch(endpoint, {
        headers: {
          Authorization: `Bearer ${localStorage.getItem("access_token")}`,
        },
      })
      const data = await response.json()
      setDiagram(data.diagram)
    } catch (error) {
      console.error("Failed to generate diagram:", error)
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex gap-2">
        <Button
          variant={diagramType === "component" ? "default" : "outline"}
          onClick={() => setDiagramType("component")}
        >
          Component Diagram
        </Button>
        <Button
          variant={diagramType === "dependencies" ? "default" : "outline"}
          onClick={() => setDiagramType("dependencies")}
        >
          Dependencies
        </Button>
      </div>

      <Button onClick={generateDiagram} disabled={loading}>
        {loading ? "Generating..." : "Generate Diagram"}
      </Button>

      {diagram && (
        <div className="mt-4 p-4 border rounded bg-slate-50 dark:bg-slate-900">
          <pre className="text-sm overflow-x-auto">{diagram}</pre>
        </div>
      )}
    </div>
  )
}

function READMEGenerator({ repositoryId }: { repositoryId: string }) {
  const [readme, setReadme] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  const generateREADME = async () => {
    setLoading(true)
    try {
      const response = await fetch(
        `/api/v1/docs/generate/readme/${repositoryId}?repository_name=test/repo&language=go`,
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${localStorage.getItem("access_token")}`,
          },
        }
      )
      const data = await response.json()
      setReadme(data.content)
    } catch (error) {
      console.error("Failed to generate README:", error)
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="space-y-4">
      <Button onClick={generateREADME} disabled={loading}>
        {loading ? "Generating..." : "Generate README"}
      </Button>

      {readme && (
        <div className="space-y-2">
          <div className="flex justify-end">
            <Button variant="outline" size="sm">
              <Download className="h-4 w-4 mr-2" />
              Download
            </Button>
          </div>
          <div className="p-4 border rounded bg-slate-50 dark:bg-slate-900 max-h-96 overflow-y-auto">
            <pre className="text-sm whitespace-pre-wrap">{readme}</pre>
          </div>
        </div>
      )}
    </div>
  )
}
