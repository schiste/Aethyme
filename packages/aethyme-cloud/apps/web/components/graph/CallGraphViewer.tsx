"use client"

import "reactflow/dist/style.css"

import { GitBranch, Loader2 } from "lucide-react"
import { useCallback, useEffect, useState } from "react"
import ReactFlow, {
  Background,
  Controls,
  type Edge,
  MarkerType,
  type Node,
  Position,
  useEdgesState,
  useNodesState,
} from "reactflow"

import { Button } from "@/components/ui/button"
import { Card } from "@/components/ui/card"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"

interface GraphData {
  nodes: Array<{
    id: string
    label: string
    type: string
    depth: number
    file?: string
    kind?: string
  }>
  edges: Array<{
    source: string
    target: string
    type: string
    file?: string
    line?: number
  }>
  root: string
  stats?: {
    total_nodes: number
    total_edges: number
    max_depth: number
  }
}

interface CallGraphViewerProps {
  symbolId: string
  initialDirection?: "callers" | "callees" | "both"
  initialMaxDepth?: number
}

export function CallGraphViewer({
  symbolId,
  initialDirection = "both",
  initialMaxDepth = 3,
}: CallGraphViewerProps) {
  const [direction, setDirection] = useState(initialDirection)
  const [maxDepth, setMaxDepth] = useState(initialMaxDepth)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [graphData, setGraphData] = useState<GraphData | null>(null)
  const [nodes, setNodes, onNodesChange] = useNodesState([])
  const [edges, setEdges, onEdgesChange] = useEdgesState([])

  const convertToReactFlow = useCallback((data: GraphData) => {
    // Convert nodes
    const reactFlowNodes: Node[] = data.nodes.map((node, index) => {
      const isRoot = node.id === data.root
      const isCaller = node.type === "caller"
      const _isCallee = node.type === "callee"

      return {
        id: node.id,
        type: "default",
        data: {
          label: (
            <div className="text-center">
              <div className="font-semibold text-sm">{node.label}</div>
              {node.file && (
                <div className="text-xs text-muted-foreground truncate max-w-[200px]">
                  {node.file}
                </div>
              )}
            </div>
          ),
        },
        position: {
          x: isRoot ? 400 : isCaller ? 200 + (node.depth * 150) : 600 + (node.depth * 150),
          y: index * 80,
        },
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
        style: {
          background: isRoot
            ? "hsl(var(--primary))"
            : isCaller
            ? "hsl(var(--blue))"
            : "hsl(var(--green))",
          color: isRoot ? "hsl(var(--primary-foreground))" : "white",
          border: "2px solid",
          borderColor: isRoot ? "hsl(var(--primary))" : "transparent",
          borderRadius: "8px",
          padding: "10px",
          width: 220,
        },
      }
    })

    // Convert edges
    const reactFlowEdges: Edge[] = data.edges.map((edge) => ({
      id: `${edge.source}-${edge.target}`,
      source: edge.source,
      target: edge.target,
      type: "smoothstep",
      animated: true,
      markerEnd: {
        type: MarkerType.ArrowClosed,
        color: "hsl(var(--muted-foreground))",
      },
      style: {
        stroke: "hsl(var(--muted-foreground))",
        strokeWidth: 2,
      },
      label: edge.line ? `line ${edge.line}` : undefined,
      labelStyle: {
        fill: "hsl(var(--muted-foreground))",
        fontSize: 10,
      },
    }))

    setNodes(reactFlowNodes)
    setEdges(reactFlowEdges)
  }, [setNodes, setEdges])

  const fetchGraph = useCallback(async () => {
    setLoading(true)
    setError(null)

    try {
      const response = await fetch(
        `/api/v1/graph/call-graph/${encodeURIComponent(symbolId)}?direction=${direction}&max_depth=${maxDepth}`,
        {
          headers: {
            Authorization: `Bearer ${localStorage.getItem("access_token")}`,
          },
        }
      )

      if (!response.ok) {
        throw new Error("Failed to fetch call graph")
      }

      const data = await response.json()
      setGraphData(data)

      // Convert to React Flow format
      convertToReactFlow(data)
    } catch (err) {
      setError(err instanceof Error ? err.message : "An error occurred")
    } finally {
      setLoading(false)
    }
  }, [symbolId, direction, maxDepth, convertToReactFlow])

  useEffect(() => {
    fetchGraph()
  }, [fetchGraph])

  if (loading) {
    return (
      <div className="flex items-center justify-center h-96">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (error) {
    return (
      <Card className="p-6">
        <div className="text-center text-destructive">
          <p className="font-semibold">Error loading call graph</p>
          <p className="text-sm mt-2">{error}</p>
          <Button onClick={fetchGraph} className="mt-4">
            Retry
          </Button>
        </div>
      </Card>
    )
  }

  return (
    <div className="space-y-4">
      {/* Controls */}
      <Card className="p-4">
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-2">
            <GitBranch className="h-4 w-4 text-muted-foreground" />
            <span className="text-sm font-medium">Call Graph</span>
          </div>

          <Select value={direction} onValueChange={(v) => setDirection(v as typeof direction)}>
            <SelectTrigger className="w-[180px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="callers">Callers (who calls this)</SelectItem>
              <SelectItem value="callees">Callees (what this calls)</SelectItem>
              <SelectItem value="both">Both</SelectItem>
            </SelectContent>
          </Select>

          <Select value={maxDepth.toString()} onValueChange={(v) => setMaxDepth(Number(v))}>
            <SelectTrigger className="w-[140px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {[1, 2, 3, 4, 5].map((d) => (
                <SelectItem key={d} value={d.toString()}>
                  Depth: {d}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          {graphData?.stats && (
            <div className="ml-auto text-sm text-muted-foreground">
              {graphData.stats.total_nodes} nodes · {graphData.stats.total_edges} edges
            </div>
          )}
        </div>
      </Card>

      {/* Graph */}
      <Card className="h-[600px]">
        <ReactFlow
          nodes={nodes}
          edges={edges}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          fitView
          attributionPosition="bottom-left"
        >
          <Controls />
          <Background />
        </ReactFlow>
      </Card>

      {/* Legend */}
      <Card className="p-4">
        <div className="flex items-center gap-6 text-sm">
          <div className="flex items-center gap-2">
            <div className="w-4 h-4 rounded bg-primary" />
            <span>Current Symbol</span>
          </div>
          <div className="flex items-center gap-2">
            <div className="w-4 h-4 rounded bg-blue-500" />
            <span>Callers</span>
          </div>
          <div className="flex items-center gap-2">
            <div className="w-4 h-4 rounded bg-green-500" />
            <span>Callees</span>
          </div>
        </div>
      </Card>
    </div>
  )
}
