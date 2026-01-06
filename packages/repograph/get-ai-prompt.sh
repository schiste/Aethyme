#!/bin/bash

# Quick script to generate AI assistant prompt with RepoGraph instructions
# Usage: ./get-ai-prompt.sh

set -e

echo "🤖 Generating AI Assistant Prompt for RepoGraph..."
echo ""

# Get token
echo "Getting authentication token..."
TOKEN=$(curl -s -X POST http://localhost:8001/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email": "test@example.com", "password": "test1234"}' | jq -r .access_token)

if [ -z "$TOKEN" ] || [ "$TOKEN" == "null" ]; then
    echo "❌ Failed to get token. Is RepoGraph API running at http://localhost:8001?"
    exit 1
fi

echo "✅ Token obtained"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "📋 COPY THIS PROMPT TO YOUR AI ASSISTANT:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

cat << EOF
🤖 CODE ASSISTANT INSTRUCTIONS:

You are working with a codebase that has RepoGraph running.
- API URL: http://localhost:8001
- Auth Token: $TOKEN

IMPORTANT: Use RepoGraph API for ALL code searches and analysis.

Available Tools:

1. SEARCH for symbols (classes, functions, methods):
   curl -X POST http://localhost:8001/api/v1/search/ \\
     -H "Authorization: Bearer $TOKEN" \\
     -H "Content-Type: application/json" \\
     -d '{"query": "SymbolName", "limit": 10}'

   Returns: Exact locations with line numbers, file paths, and symbol types.
   USE THIS INSTEAD OF: grep, find, or reading entire files to locate code.

2. GET RELATIONSHIPS (what connects to what):
   curl -X POST http://localhost:8001/api/v1/ego/ \\
     -H "Authorization: Bearer $TOKEN" \\
     -H "Content-Type: application/json" \\
     -d '{"symbol": "file.py:ClassName", "depth": 2}'

   Returns: All connected code organized by relationship depth.
   USE THIS INSTEAD OF: Reading multiple files to understand dependencies.

3. ANALYZE IMPACT (what breaks if you change something):
   curl -X POST http://localhost:8001/api/v1/impact/ \\
     -H "Authorization: Bearer $TOKEN" \\
     -H "Content-Type: application/json" \\
     -d '{"symbols": ["file.py:ClassName"]}'

   Returns: All code that depends on this symbol.
   USE THIS BEFORE: Suggesting any refactoring or changes.

RULES:
✅ ALWAYS search with RepoGraph before reading files
✅ ALWAYS get ego graph to understand relationships
✅ ALWAYS run impact analysis before suggesting changes
❌ NEVER use grep/find when RepoGraph can answer it
❌ NEVER read entire files when you can query the graph

Example Workflow:
User: "Find the GraphStore class"
You: curl -X POST http://localhost:8001/api/v1/search/ -H "Authorization: Bearer $TOKEN" -d '{"query":"GraphStore"}'
You: Found at graph/store.py:17 (class)

NOT:
You: grep -r "class GraphStore"
EOF

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "💡 Tips:"
echo "   • Copy everything from '🤖 CODE ASSISTANT' to the end"
echo "   • Paste at the start of your AI conversation"
echo "   • The AI will now use RepoGraph instead of grep/file reads"
echo "   • Token expires in 24 hours - run this script again if needed"
echo ""
echo "📊 Quick test: Ask your AI 'Find the GraphStore class'"
echo "   - With RepoGraph: 1 API call, instant result"
echo "   - Without: grep, read files, guess location"
echo ""
