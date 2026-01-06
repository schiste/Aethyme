"use client"

import { ArrowLeftRight,GitBranch, Network } from "lucide-react"
import { useParams } from "next/navigation"

import { CallGraphViewer } from "@/components/graph/CallGraphViewer"
import { ClassHierarchyTree } from "@/components/graph/ClassHierarchyTree"
import { Card } from "@/components/ui/card"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"

export default function GraphPage() {
  const params = useParams()
  const symbolId = params.symbolId as string

  return (
    <div className="space-y-6 p-6">
      <div>
        <h1 className="text-3xl font-bold">Code Graph Analysis</h1>
        <p className="text-muted-foreground mt-1">
          Explore relationships and dependencies for <code className="text-sm bg-muted px-2 py-1 rounded">{decodeURIComponent(symbolId)}</code>
        </p>
      </div>

      <Tabs defaultValue="call-graph" className="w-full">
        <TabsList className="grid w-full max-w-md grid-cols-3">
          <TabsTrigger value="call-graph" className="flex items-center gap-2">
            <GitBranch className="h-4 w-4" />
            Call Graph
          </TabsTrigger>
          <TabsTrigger value="hierarchy" className="flex items-center gap-2">
            <Network className="h-4 w-4" />
            Hierarchy
          </TabsTrigger>
          <TabsTrigger value="impact" className="flex items-center gap-2">
            <ArrowLeftRight className="h-4 w-4" />
            Impact
          </TabsTrigger>
        </TabsList>

        <TabsContent value="call-graph" className="mt-6">
          <CallGraphViewer symbolId={decodeURIComponent(symbolId)} />
        </TabsContent>

        <TabsContent value="hierarchy" className="mt-6">
          <ClassHierarchyTree symbolId={decodeURIComponent(symbolId)} />
        </TabsContent>

        <TabsContent value="impact" className="mt-6">
          <Card className="p-6">
            <h3 className="text-lg font-semibold mb-4">Impact Analysis</h3>
            <p className="text-muted-foreground">
              Analyzing what would break if you change this symbol...
            </p>
            {/* Impact analysis component will be added here */}
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  )
}
