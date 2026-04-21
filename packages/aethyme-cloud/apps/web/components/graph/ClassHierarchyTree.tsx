"use client"

import "reactflow/dist/style.css"

import { Loader2, Network } from "lucide-react"
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

interface HierarchyData {
  nodes: Array<{
    id: string
    label: string
    type: string
    level: number
    file?: string
  }>
  edges: Array<{
    source: string
    target: string
    type: string
  }>
  root: string
}

interface ClassHierarchyTreeProps {
  symbolId: string
  initialDirection?: "parents" | "children" | "both"
}

export function ClassHierarchyTree({
  symbolId,
  initialDirection = "both",
}: ClassHierarchyTreeProps) {
  const [direction, setDirection] = useState(initialDirection)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [hierarchyData, setHierarchyData] = useState<HierarchyData | null>(null)
  const [nodes, setNodes, onNodesChange] = useNodesState([])
  const [edges, setEdges, onEdgesChange] = useEdgesState([])

  const convertToReactFlow = useCallback((data: HierarchyData) => {
    // Sort nodes by level
    const sortedNodes = [...data.nodes].sort((a, b) => b.level - a.level)

    // Convert nodes to React Flow format with vertical hierarchy
    const reactFlowNodes: Node[] = sortedNodes.map((node, index) => {
      const isRoot = node.id === data.root
      const isParent = node.type === "parent"
      const _isChild = node.type === "child"

      return {
        id: node.id,
        type: "default",
        data: {
          label: (
            <div className="text-center">
              <div className="font-semibold text-sm">{node.label}</div>
              {node.file && (
                <div className="text-xs text-muted-foreground truncate max-w-[180px]">
                  {node.file}
                </div>
              )}
            </div>
          ),
        },
        position: {
          x: index * 250,
          y: node.level * 120,
        },
        sourcePosition: Position.Bottom,
        targetPosition: Position.Top,
        style: {
          background: isRoot
            ? "hsl(var(--primary))"
            : isParent
            ? "hsl(var(--blue))"
            : "hsl(var(--green))",
          color: isRoot ? "hsl(var(--primary-foreground))" : "white",
          border: "2px solid",
          borderColor: isRoot ? "hsl(var(--primary))" : "transparent",
          borderRadius: "8px",
          padding: "10px",
          width: 200,
        },
      }
    })

    // Convert edges
    const reactFlowEdges: Edge[] = data.edges.map((edge) => ({
      id: `${edge.source}-${edge.target}`,
      source: edge.source,
      target: edge.target,
      type: "smoothstep",
      animated: edge.type === "inherits",
      markerEnd: {
        type: MarkerType.ArrowClosed,
        color: "hsl(var(--muted-foreground))",
      },
      style: {
        stroke:
          edge.type === "inherits"
            ? "hsl(var(--primary))"
            : "hsl(var(--muted-foreground))",
        strokeWidth: 2,
        strokeDasharray: edge.type === "implements" ? "5 5" : undefined,
      },
      label: edge.type,
      labelStyle: {
        fill: "hsl(var(--muted-foreground))",
        fontSize: 10,
      },
    }))

    setNodes(reactFlowNodes)
    setEdges(reactFlowEdges)
  }, [setNodes, setEdges])

  const fetchHierarchy = useCallback(async () => {
    setLoading(true)
    setError(null)

    try {
      const response = await fetch(
        `/api/v1/graph/class-hierarchy/${encodeURIComponent(symbolId)}?direction=${direction}`,
        {
          headers: {
            Authorization: `Bearer ${localStorage.getItem("access_token")}`,
          },
        }
      )

      if (!response.ok) {
        throw new Error("Failed to fetch class hierarchy")
      }

      const data = await response.json()
      setHierarchyData(data)

      // Convert to React Flow format
      convertToReactFlow(data)
    } catch (err) {
      setError(err instanceof Error ? err.message : "An error occurred")
    } finally {
      setLoading(false)
    }
  }, [symbolId, direction, convertToReactFlow])

  useEffect(() => {
    fetchHierarchy()
  }, [fetchHierarchy])

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
          <p className="font-semibold">Error loading class hierarchy</p>
          <p className="text-sm mt-2">{error}</p>
          <Button onClick={fetchHierarchy} className="mt-4">
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
            <Network className="h-4 w-4 text-muted-foreground" />
            <span className="text-sm font-medium">Class Hierarchy</span>
          </div>

          <Select value={direction} onValueChange={(v) => setDirection(v as typeof direction)}>
            <SelectTrigger className="w-[200px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="parents">Parents (superclasses)</SelectItem>
              <SelectItem value="children">Children (subclasses)</SelectItem>
              <SelectItem value="both">Both</SelectItem>
            </SelectContent>
          </Select>

          {hierarchyData && (
            <div className="ml-auto text-sm text-muted-foreground">
              {hierarchyData.nodes.length} classes
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
            <span>Current Class</span>
          </div>
          <div className="flex items-center gap-2">
            <div className="w-4 h-4 rounded bg-blue-500" />
            <span>Parents</span>
          </div>
          <div className="flex items-center gap-2">
            <div className="w-4 h-4 rounded bg-green-500" />
            <span>Children</span>
          </div>
          <div className="flex items-center gap-2">
            <div className="w-12 h-0.5 bg-primary" />
            <span>Inherits</span>
          </div>
          <div className="flex items-center gap-2">
            <div className="w-12 h-0.5 border-t-2 border-dashed border-muted-foreground" />
            <span>Implements</span>
          </div>
        </div>
      </Card>
    </div>
  )
}
