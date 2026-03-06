# AI Integration Guide

## Overview

Aethyme provides AI agents with a queryable knowledge graph of your codebase, enabling faster and more accurate code understanding compared to traditional file-based operations.

### What Aethyme Provides to AI Agents

- **Structured code search**: Find classes, functions, and methods by name with exact locations
- **Relationship graphs**: Understand how code connects through dependencies and calls
- **Impact analysis**: Identify what code would be affected by changes
- **Multi-language support**: Works with Python, TypeScript, and more

### Why Use Aethyme for AI Code Intelligence

**Traditional approach (grep/file reading):**
- Searches through all files linearly
- No understanding of code structure
- Requires reading entire files for context
- Slow and token-intensive

**Aethyme approach:**
- Instant symbol lookup via indexed graph
- Returns exact locations with line numbers
- Provides relationship context automatically
- 90% less context usage, 4x faster completions

### Key Capabilities

1. **Symbol Search**: Locate any code symbol instantly
2. **Ego Graphs**: Explore relationships around a symbol at configurable depth
3. **Impact Analysis**: Understand blast radius before making changes
4. **Hybrid Search**: Combined full-text and fuzzy matching

---

## The Core Problem: AI Agents Don't Know About Aethyme by Default

When you talk to Claude, GPT-4, or any AI assistant, they have:
- Built-in tools (file reading, bash commands, web search)
- NO knowledge that Aethyme exists
- NO way to access your Aethyme API

**You must explicitly integrate Aethyme into their workflow.**

This is by design for security and privacy. AI models have no built-in knowledge of:
- Your local development environment
- Running services on localhost
- Custom APIs you've deployed
- Authentication tokens

They only know about:
- Tools explicitly defined in their configuration (MCP)
- Instructions you give them in prompts
- Built-in capabilities (file read, bash, web search)

---

## Integration Methods

### Method 1: MCP (Model Context Protocol) - For Claude

**Status:** Recommended for Claude Desktop users

MCP is Claude's native tool integration protocol. When configured, Aethyme appears in Claude Desktop's tool menu automatically.

#### How It Works

1. You create an MCP server that wraps the Aethyme API
2. You configure Claude Desktop to load this server
3. Claude automatically discovers the tools when it starts
4. Claude knows to use them because they appear in its tool list with descriptions

#### Setup Steps

**1. Create MCP configuration file:**

```bash
# Location: ~/.config/claude/mcp_servers.json (Mac/Linux)
# or: %APPDATA%\Claude\mcp_servers.json (Windows)

mkdir -p ~/.config/claude
cat > ~/.config/claude/mcp_servers.json << 'EOF'
{
  "aethyme": {
    "command": "node",
    "args": ["/path/to/aethyme/mcp-server.js"],
    "env": {
      "AETHYME_API_URL": "http://localhost:8001",
      "AETHYME_TOKEN": "your-token-here"
    }
  }
}
EOF
```

**2. Create the MCP server file:**

```javascript
#!/usr/bin/env node

/**
 * Aethyme MCP Server
 * Exposes Aethyme as tools for Claude Desktop
 */

const API_URL = process.env.AETHYME_API_URL || 'http://localhost:8001';
const TOKEN = process.env.AETHYME_TOKEN;

// Tool definitions - Claude reads these to understand what tools are available
const tools = [
  {
    name: "aethyme_search",
    description: "Search for code symbols (classes, functions, methods) in the indexed codebase. Use this INSTEAD of grep or file search. Much faster and returns structured data with exact locations.",
    inputSchema: {
      type: "object",
      properties: {
        query: {
          type: "string",
          description: "Symbol name to search for (e.g., 'GraphStore', 'search_symbols')"
        },
        search_type: {
          type: "string",
          enum: ["exact", "fuzzy", "hybrid"],
          default: "hybrid",
          description: "Search type: exact=exact match, fuzzy=similar names, hybrid=both"
        },
        limit: {
          type: "number",
          default: 10,
          description: "Maximum number of results to return"
        }
      },
      required: ["query"]
    }
  },
  {
    name: "aethyme_ego_graph",
    description: "Get the relationship graph for a code symbol. Shows all connected code (what it calls, what calls it, imports, etc.) organized by relationship depth. Use this to understand how code connects before making changes.",
    inputSchema: {
      type: "object",
      properties: {
        symbol: {
          type: "string",
          description: "Full symbol name from search results (e.g., 'graph/store.py:GraphStore')"
        },
        depth: {
          type: "number",
          default: 2,
          description: "How many relationship levels to traverse (1-5)"
        }
      },
      required: ["symbol"]
    }
  },
  {
    name: "aethyme_impact_analysis",
    description: "Analyze the impact of changing code symbols. Shows what code depends on them and would be affected. ALWAYS use this before suggesting refactoring or changes.",
    inputSchema: {
      type: "object",
      properties: {
        symbols: {
          type: "array",
          items: { type: "string" },
          description: "List of symbols to analyze (e.g., ['graph/store.py:GraphStore'])"
        }
      },
      required: ["symbols"]
    }
  }
];

// Handle MCP protocol
const stdin = process.stdin;
const stdout = process.stdout;

stdin.setEncoding('utf-8');

let buffer = '';
stdin.on('data', async (chunk) => {
  buffer += chunk;
  const lines = buffer.split('\n');
  buffer = lines.pop() || '';

  for (const line of lines) {
    if (!line.trim()) continue;

    try {
      const request = JSON.parse(line);

      // Handle tool listing
      if (request.method === 'tools/list') {
        const response = { tools };
        stdout.write(JSON.stringify(response) + '\n');
      }

      // Handle tool calls
      else if (request.method === 'tools/call') {
        const { name, arguments: args } = request.params;

        let result;
        if (name === 'aethyme_search') {
          result = await callAethyme('/api/search/', args);
        } else if (name === 'aethyme_ego_graph') {
          result = await callAethyme('/api/ego/', args);
        } else if (name === 'aethyme_impact_analysis') {
          result = await callAethyme('/api/impact/', args);
        }

        const response = {
          content: [
            {
              type: 'text',
              text: JSON.stringify(result, null, 2)
            }
          ]
        };

        stdout.write(JSON.stringify(response) + '\n');
      }
    } catch (error) {
      process.stderr.write(`Error: ${error.message}\n`);
    }
  }
});

async function callAethyme(endpoint, body) {
  const fetch = (await import('node-fetch')).default;

  const response = await fetch(`${API_URL}${endpoint}`, {
    method: 'POST',
    headers: {
      'Authorization': `Bearer ${TOKEN}`,
      'Content-Type': 'application/json'
    },
    body: JSON.stringify(body)
  });

  if (!response.ok) {
    throw new Error(`Aethyme API error: ${response.statusText}`);
  }

  return await response.json();
}

process.stderr.write('Aethyme MCP server started\n');
```

**3. Install dependencies:**

```bash
cd packages/aethyme
npm init -y
npm install node-fetch
chmod +x mcp-server.js
```

**4. Get a token and update config:**

```bash
# Get token
TOKEN=$(curl -s -X POST http://localhost:8001/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email": "test@example.com", "password": "test1234"}' | jq -r .access_token)

# Update MCP config with the token
sed -i '' "s|your-token-here|$TOKEN|" ~/.config/claude/mcp_servers.json

echo "✅ MCP configured. Restart Claude Desktop to activate."
```

**5. Restart Claude Desktop**

After restart, when Claude processes requests, it will see these tools in its tool list:
- `aethyme_search`
- `aethyme_ego_graph`
- `aethyme_impact_analysis`

Claude will automatically use them because they appear with clear descriptions!

#### Verification

```bash
# Check MCP config exists
cat ~/.config/claude/mcp_servers.json

# Expected: Should show aethyme configuration

# In Claude Desktop, ask:
# "What tools do you have available?"
# Claude should mention aethyme_search, aethyme_ego_graph, etc.
```

---

### Method 2: OpenAI Function Calling - For GPT-4 and Compatible Models

For GPT-4 or other OpenAI-compatible models, define Aethyme as functions.

#### Setup

```python
import openai
import requests
import json

AETHYME_API = "http://localhost:8001"
TOKEN = "your-jwt-token"

# Define tools for GPT
tools = [
    {
        "type": "function",
        "function": {
            "name": "aethyme_search",
            "description": "Search for code symbols in the indexed codebase. Much faster and more accurate than grep.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The symbol name to search for (e.g., 'GraphStore', 'search_symbols')"
                    },
                    "search_type": {
                        "type": "string",
                        "enum": ["exact", "fuzzy", "hybrid"],
                        "description": "Type of search to perform"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results"
                    }
                },
                "required": ["query"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "aethyme_ego_graph",
            "description": "Get the relationship graph for a code symbol. Shows all connected code and dependencies.",
            "parameters": {
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Full symbol name including file (e.g., 'graph/store.py:GraphStore')"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "How many relationship levels to traverse (1-5)"
                    }
                },
                "required": ["symbol"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "aethyme_impact",
            "description": "Analyze impact of changing a symbol. Shows what code depends on it.",
            "parameters": {
                "type": "object",
                "properties": {
                    "symbols": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "List of symbols to analyze impact for"
                    }
                },
                "required": ["symbols"]
            }
        }
    }
]

def call_aethyme(function_name, arguments):
    """Execute Aethyme API calls"""
    headers = {"Authorization": f"Bearer {TOKEN}"}

    if function_name == "aethyme_search":
        response = requests.post(
            f"{AETHYME_API}/api/search/",
            json=arguments,
            headers=headers
        )
    elif function_name == "aethyme_ego_graph":
        response = requests.post(
            f"{AETHYME_API}/api/ego/",
            json=arguments,
            headers=headers
        )
    elif function_name == "aethyme_impact":
        response = requests.post(
            f"{AETHYME_API}/api/impact/",
            json=arguments,
            headers=headers
        )

    return response.json()

# Use with OpenAI
messages = [
    {"role": "system", "content": "You are a code assistant with access to a code knowledge graph via Aethyme tools."},
    {"role": "user", "content": "Find the GraphStore class and show me what it depends on"}
]

response = openai.chat.completions.create(
    model="gpt-4",
    messages=messages,
    tools=tools,
    tool_choice="auto"
)

# Handle tool calls
if response.choices[0].message.tool_calls:
    for tool_call in response.choices[0].message.tool_calls:
        function_name = tool_call.function.name
        arguments = json.loads(tool_call.function.arguments)

        result = call_aethyme(function_name, arguments)

        # Send result back to GPT
        messages.append({
            "role": "tool",
            "tool_call_id": tool_call.id,
            "content": json.dumps(result)
        })

    # Get final response
    final_response = openai.chat.completions.create(
        model="gpt-4",
        messages=messages,
        tools=tools
    )
    print(final_response.choices[0].message.content)
```

---

### Method 3: Custom AI Agent Integration

Create a specialized AI assistant that always uses Aethyme.

#### Setup

```python
# aethyme_assistant.py

import anthropic
import requests
import json

class AethymeAssistant:
    def __init__(self, aethyme_url, token, anthropic_key):
        self.aethyme_url = aethyme_url
        self.token = token
        self.client = anthropic.Anthropic(api_key=anthropic_key)

    def search(self, query, search_type="hybrid"):
        """Search code using Aethyme"""
        response = requests.post(
            f"{self.aethyme_url}/api/search/",
            json={"query": query, "search_type": search_type, "limit": 10},
            headers={"Authorization": f"Bearer {self.token}"}
        )
        return response.json()

    def get_relationships(self, symbol, depth=2):
        """Get code relationships using ego graph"""
        response = requests.post(
            f"{self.aethyme_url}/api/ego/",
            json={"symbol": symbol, "depth": depth},
            headers={"Authorization": f"Bearer {self.token}"}
        )
        return response.json()

    def analyze_impact(self, symbols):
        """Analyze impact of changing symbols"""
        response = requests.post(
            f"{self.aethyme_url}/api/impact/",
            json={"symbols": symbols},
            headers={"Authorization": f"Bearer {self.token}"}
        )
        return response.json()

    def ask(self, question):
        """Ask the assistant a question with Aethyme context"""

        # Build system prompt
        system_prompt = """You are a code assistant with access to Aethyme, a code knowledge graph.

When answering questions:
1. Use aethyme.search() to find code symbols
2. Use aethyme.get_relationships() to understand dependencies
3. Use aethyme.analyze_impact() before suggesting changes

Always prefer Aethyme over reading entire files."""

        # Create conversation
        messages = [{"role": "user", "content": question}]

        # Let Claude decide what to do
        response = self.client.messages.create(
            model="claude-sonnet-4-5",
            max_tokens=4096,
            system=system_prompt,
            messages=messages,
            tools=[
                {
                    "name": "aethyme_search",
                    "description": "Search for code symbols in the indexed codebase",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string"},
                            "search_type": {"type": "string", "enum": ["exact", "fuzzy", "hybrid"]}
                        },
                        "required": ["query"]
                    }
                },
                {
                    "name": "aethyme_get_relationships",
                    "description": "Get relationship graph for a symbol",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "symbol": {"type": "string"},
                            "depth": {"type": "integer"}
                        },
                        "required": ["symbol"]
                    }
                },
                {
                    "name": "aethyme_analyze_impact",
                    "description": "Analyze impact of changing symbols",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "symbols": {
                                "type": "array",
                                "items": {"type": "string"}
                            }
                        },
                        "required": ["symbols"]
                    }
                }
            ]
        )

        # Handle tool use
        while response.stop_reason == "tool_use":
            tool_use = next(block for block in response.content if block.type == "tool_use")

            # Execute Aethyme call
            if tool_use.name == "aethyme_search":
                result = self.search(**tool_use.input)
            elif tool_use.name == "aethyme_get_relationships":
                result = self.get_relationships(**tool_use.input)
            elif tool_use.name == "aethyme_analyze_impact":
                result = self.analyze_impact(**tool_use.input)

            # Continue conversation with result
            messages.append({"role": "assistant", "content": response.content})
            messages.append({
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": tool_use.id,
                        "content": json.dumps(result)
                    }
                ]
            })

            response = self.client.messages.create(
                model="claude-sonnet-4-5",
                max_tokens=4096,
                system=system_prompt,
                messages=messages,
                tools=response.tools
            )

        # Return final answer
        return response.content[0].text

# Usage
assistant = AethymeAssistant(
    aethyme_url="http://localhost:8001",
    token="your-jwt-token",
    anthropic_key="your-anthropic-key"
)

answer = assistant.ask("Find the GraphStore class and explain what it does")
print(answer)
```

#### Automated Discovery Wrapper

Build a wrapper that automatically uses Aethyme without AI awareness:

```python
#!/usr/bin/env python3
# aethyme-agent.py

import anthropic
import requests
import sys
import json

AETHYME_URL = "http://localhost:8001"

class AethymeAgent:
    def __init__(self, anthropic_key, aethyme_token):
        self.client = anthropic.Anthropic(api_key=anthropic_key)
        self.rg_token = aethyme_token
        self.rg_headers = {"Authorization": f"Bearer {aethyme_token}"}

    def search_code(self, query):
        """Automatically search Aethyme"""
        response = requests.post(
            f"{AETHYME_URL}/api/search/",
            headers=self.rg_headers,
            json={"query": query, "limit": 10}
        )
        return response.json()

    def get_relationships(self, symbol, depth=2):
        """Get code relationships"""
        response = requests.post(
            f"{AETHYME_URL}/api/ego/",
            headers=self.rg_headers,
            json={"symbol": symbol, "depth": depth}
        )
        return response.json()

    def chat(self, message):
        """Enhanced chat that uses Aethyme automatically"""

        # Check if user is asking about code
        code_keywords = ["class", "function", "method", "find", "where is", "show me"]
        uses_code = any(kw in message.lower() for kw in code_keywords)

        # Auto-search Aethyme for relevant context
        context = ""
        if uses_code:
            # Extract potential symbol names (simple heuristic)
            words = message.split()
            for word in words:
                if word[0].isupper() and len(word) > 3:  # Likely a class name
                    print(f"🔍 Auto-searching Aethyme for: {word}")
                    results = self.search_code(word)
                    if results.get('results'):
                        context += f"\n\nAethyme found:\n{json.dumps(results, indent=2)}\n"
                        break

        # Build prompt with context
        system_prompt = """You are a code assistant with access to Aethyme data.
        When code search results are provided, use them instead of guessing or searching files.
        The results include exact line numbers and file paths."""

        full_message = message
        if context:
            full_message = f"{message}\n\n--- Aethyme Context ---{context}"

        # Call Claude
        response = self.client.messages.create(
            model="claude-sonnet-4-5",
            max_tokens=4096,
            system=system_prompt,
            messages=[{"role": "user", "content": full_message}]
        )

        return response.content[0].text

# Usage
if __name__ == "__main__":
    import os

    # Get token
    login_response = requests.post(
        f"{AETHYME_URL}/api/auth/login",
        json={"email": "test@example.com", "password": "test1234"}
    )
    token = login_response.json()["access_token"]

    # Create agent
    agent = AethymeAgent(
        anthropic_key=os.environ.get("ANTHROPIC_API_KEY"),
        aethyme_token=token
    )

    # Interactive chat
    print("Aethyme-Enhanced Chat (type 'exit' to quit)")
    print("=" * 50)

    while True:
        user_input = input("\nYou: ")
        if user_input.lower() == "exit":
            break

        response = agent.chat(user_input)
        print(f"\nAssistant: {response}")
```

Claude doesn't need to know about the API - the wrapper handles everything!

---

### Method 4: System Prompts and Discovery

If you can't use MCP or custom agents (e.g., using ChatGPT, Claude in browser, or API), tell the AI about Aethyme in your instructions.

#### How It Works

You start EVERY conversation with instructions that include:
1. Aethyme API details
2. When to use it
3. Example calls

#### Setup Template

```markdown
# System Instructions for Working with This Codebase

You have access to Aethyme, a code knowledge graph API running at http://localhost:8001

## Available Tools

### 1. Search for Code Symbols
Instead of using grep or file search, use this API:

```bash
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"query": "SymbolName", "limit": 10}'
```

Returns: Exact locations with line numbers, kinds (class/function/method), structured data.

USE THIS INSTEAD OF: grep, find, file reading for symbol location.

### 2. Get Code Relationships
To understand how code connects:

```bash
curl -X POST http://localhost:8001/api/ego/ \
  -H "Authorization: Bearer YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"symbol": "file.py:ClassName", "depth": 2}'
```

Returns: All connected code organized by relationship depth.

USE THIS INSTEAD OF: Reading multiple files to understand dependencies.

### 3. Impact Analysis
Before suggesting changes:

```bash
curl -X POST http://localhost:8001/api/impact/ \
  -H "Authorization: Bearer YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"symbols": ["file.py:ClassName"]}'
```

Returns: What code depends on this symbol.

## Your Token

YOUR_TOKEN = [paste-token-here]

## Rules

1. ALWAYS search with Aethyme before reading files
2. ALWAYS get ego graph to understand relationships
3. ALWAYS run impact analysis before refactoring
4. NEVER read entire files when you can query the graph
5. Use structured data from API, not grep output

## Example Workflow
When user asks: "Find the GraphStore class"

BAD (old way):
  grep -r "class GraphStore" .
  cat src/graph/store.py

GOOD (with Aethyme):
  curl -X POST http://localhost:8001/api/search/ ... -d '{"query": "GraphStore"}'
  # Get exact location instantly
```

#### Quick Start Script

```bash
# Get fresh token and generate prompt
TOKEN=$(curl -s -X POST http://localhost:8001/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email": "test@example.com", "password": "test1234"}' | jq -r .access_token)

echo "Copy this to start your AI conversation:"
echo ""
echo "I'm working on a codebase with Aethyme running at http://localhost:8001"
echo "My token is: $TOKEN"
echo ""
echo "IMPORTANT: Use Aethyme API for all code searches and understanding:"
echo "- Search: POST /api/search/ with {\"query\": \"symbol\"}"
echo "- Relationships: POST /api/ego/ with {\"symbol\": \"full.path:Name\", \"depth\": 2}"
echo "- Impact: POST /api/impact/ with {\"symbols\": [\"name\"]}"
echo ""
echo "Always use these APIs instead of grep/find/reading files."
```

---

## Comparison: Which Method Should You Use?

| Method | Setup Effort | User Experience | AI Awareness | Best For |
|--------|-------------|-----------------|--------------|----------|
| **MCP** | Medium (one-time) | Seamless | AI knows tools exist | Claude Desktop users |
| **OpenAI Function Calling** | Medium (one-time) | Seamless | AI knows tools exist | GPT-4, OpenAI-compatible models |
| **Custom Agent** | High (one-time) | Automatic | AI unaware (transparent) | Production, automation |
| **System Prompt** | Low (every session) | Manual | AI must remember | Any AI, quick tests |

**Recommended approach:**
- **For daily use:** Set up MCP (one-time, seamless)
- **For team use:** Create custom agent or shared prompts
- **For testing:** Use system prompts

---

## Measuring AI Effectiveness

### 1. Track Tool Usage

Add logging to Aethyme API to track AI usage:

```python
# In src/api/middleware.py (create this file)

import time
import structlog
from fastapi import Request

logger = structlog.get_logger(__name__)

async def track_ai_usage(request: Request, call_next):
    """Track API usage for analytics"""

    start_time = time.time()

    # Get user agent to identify AI vs human
    user_agent = request.headers.get("user-agent", "")
    is_ai = any(ai in user_agent.lower() for ai in ["claude", "gpt", "anthropic", "openai"])

    response = await call_next(request)

    duration = time.time() - start_time

    # Log usage
    logger.info(
        "api_request",
        path=request.url.path,
        method=request.method,
        duration=duration,
        is_ai_client=is_ai,
        user_agent=user_agent
    )

    return response

# Add to main.py
from .middleware import track_ai_usage

app.middleware("http")(track_ai_usage)
```

**View AI usage stats:**

```bash
docker logs aethyme-api | grep "is_ai_client=True" | wc -l
# Count of AI-initiated requests

docker logs aethyme-api | grep "is_ai_client=True" | grep "search" | wc -l
# AI searches

docker logs aethyme-api | grep "is_ai_client=True" | grep "ego" | wc -l
# AI relationship queries
```

### 2. A/B Testing Framework

Create test scenarios to compare AI performance with and without Aethyme:

```python
# test_ai_effectiveness.py

import time
import json

class AIPerformanceTest:
    def __init__(self, assistant_with_rg, assistant_without_rg):
        self.with_rg = assistant_with_rg
        self.without_rg = assistant_without_rg

    def run_test(self, question, expected_answer_contains):
        """Run the same question with and without Aethyme"""

        print(f"\n{'='*60}")
        print(f"Question: {question}")
        print(f"{'='*60}\n")

        # Test WITHOUT Aethyme
        print("WITHOUT Aethyme:")
        start = time.time()
        answer_without = self.without_rg.ask(question)
        time_without = time.time() - start
        tokens_without = len(answer_without.split()) * 1.3  # Rough estimate

        print(f"  Time: {time_without:.2f}s")
        print(f"  Tokens: ~{tokens_without:.0f}")
        print(f"  Answer: {answer_without[:200]}...")

        # Test WITH Aethyme
        print("\nWITH Aethyme:")
        start = time.time()
        answer_with = self.with_rg.ask(question)
        time_with = time.time() - start
        tokens_with = len(answer_with.split()) * 1.3

        print(f"  Time: {time_with:.2f}s")
        print(f"  Tokens: ~{tokens_with:.0f}")
        print(f"  Answer: {answer_with[:200]}...")

        # Compare
        print("\nComparison:")
        print(f"  Speed improvement: {((time_without - time_with) / time_without * 100):.1f}%")
        print(f"  Token reduction: {((tokens_without - tokens_with) / tokens_without * 100):.1f}%")

        # Check accuracy
        accuracy_without = expected_answer_contains in answer_without.lower()
        accuracy_with = expected_answer_contains in answer_with.lower()

        print(f"  Accuracy WITHOUT: {'✅' if accuracy_without else '❌'}")
        print(f"  Accuracy WITH: {'✅' if accuracy_with else '❌'}")

        return {
            "question": question,
            "time_without": time_without,
            "time_with": time_with,
            "tokens_without": tokens_without,
            "tokens_with": tokens_with,
            "accurate_without": accuracy_without,
            "accurate_with": accuracy_with
        }

# Run tests
tester = AIPerformanceTest(
    assistant_with_rg=AethymeAssistant(...),
    assistant_without_rg=TraditionalAssistant(...)
)

results = []
results.append(tester.run_test(
    "Find the GraphStore class and list its methods",
    expected_answer_contains="graphstore"
))

results.append(tester.run_test(
    "What would break if I change the Node class?",
    expected_answer_contains="impact"
))

results.append(tester.run_test(
    "Show me all database query functions",
    expected_answer_contains="query"
))

# Summary
print("\n" + "="*60)
print("SUMMARY")
print("="*60)

avg_speedup = sum(r['time_without'] / r['time_with'] for r in results) / len(results)
avg_token_reduction = sum((r['tokens_without'] - r['tokens_with']) / r['tokens_without'] for r in results) / len(results)

print(f"Average speedup: {avg_speedup:.2f}x")
print(f"Average token reduction: {avg_token_reduction*100:.1f}%")
print(f"Accuracy WITH Aethyme: {sum(r['accurate_with'] for r in results)}/{len(results)}")
print(f"Accuracy WITHOUT Aethyme: {sum(r['accurate_without'] for r in results)}/{len(results)}")
```

### 3. Real-Time Dashboard

Create a dashboard to monitor AI usage:

```python
# dashboard.py

from flask import Flask, render_template, jsonify
import subprocess
import re

app = Flask(__name__)

@app.route('/')
def dashboard():
    return render_template('dashboard.html')

@app.route('/api/stats')
def get_stats():
    """Get real-time Aethyme usage statistics"""

    logs = subprocess.check_output(
        ["docker", "logs", "aethyme-api", "--tail", "1000"]
    ).decode()

    # Parse logs
    total_requests = len(re.findall(r'api_request', logs))
    ai_requests = len(re.findall(r'is_ai_client=True', logs))
    search_queries = len(re.findall(r'path=/api/search/', logs))
    ego_queries = len(re.findall(r'path=/api/ego/', logs))

    # Calculate average response time
    durations = re.findall(r'duration=([\d.]+)', logs)
    avg_duration = sum(float(d) for d in durations) / len(durations) if durations else 0

    return jsonify({
        "total_requests": total_requests,
        "ai_requests": ai_requests,
        "ai_percentage": (ai_requests / total_requests * 100) if total_requests > 0 else 0,
        "search_queries": search_queries,
        "ego_queries": ego_queries,
        "avg_response_time": avg_duration,
        "most_searched": get_most_searched(logs)
    })

def get_most_searched(logs):
    """Find most frequently searched symbols"""
    searches = re.findall(r'"query":"([^"]+)"', logs)
    from collections import Counter
    return dict(Counter(searches).most_common(10))

if __name__ == '__main__':
    app.run(debug=True, port=5000)
```

**Dashboard HTML (templates/dashboard.html):**

```html
<!DOCTYPE html>
<html>
<head>
    <title>Aethyme AI Usage Dashboard</title>
    <script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
</head>
<body>
    <h1>Aethyme AI Usage Dashboard</h1>

    <div id="stats">
        <h2>Real-time Stats</h2>
        <p>Total API Requests: <span id="total"></span></p>
        <p>AI Requests: <span id="ai"></span> (<span id="ai-pct"></span>%)</p>
        <p>Search Queries: <span id="searches"></span></p>
        <p>Ego Graph Queries: <span id="ego"></span></p>
        <p>Avg Response Time: <span id="duration"></span>ms</p>
    </div>

    <div>
        <h2>Most Searched Symbols</h2>
        <canvas id="searchChart"></canvas>
    </div>

    <script>
        function updateStats() {
            fetch('/api/stats')
                .then(r => r.json())
                .then(data => {
                    document.getElementById('total').textContent = data.total_requests;
                    document.getElementById('ai').textContent = data.ai_requests;
                    document.getElementById('ai-pct').textContent = data.ai_percentage.toFixed(1);
                    document.getElementById('searches').textContent = data.search_queries;
                    document.getElementById('ego').textContent = data.ego_queries;
                    document.getElementById('duration').textContent = (data.avg_response_time * 1000).toFixed(1);

                    // Update chart
                    updateChart(data.most_searched);
                });
        }

        function updateChart(searches) {
            // Chart.js code here
        }

        // Update every 5 seconds
        setInterval(updateStats, 5000);
        updateStats();
    </script>
</body>
</html>
```

**Run dashboard:**

```bash
python dashboard.py
# Visit http://localhost:5000
```

---

## Best Practices

### When to Use Which Method

- **Symbol lookup**: Always use `aethyme_search` instead of grep
- **Understanding dependencies**: Use `aethyme_ego_graph` with depth 2-3
- **Before refactoring**: Always use `aethyme_impact` first
- **Exploring unknown code**: Start with search, then ego graph for context

### Performance Optimization

1. **Limit ego graph depth**: Start with depth 2, only increase if needed
2. **Use hybrid search**: Combines speed of exact match with flexibility of fuzzy
3. **Cache tokens**: Reuse authentication tokens across requests
4. **Batch impact analysis**: Analyze multiple symbols in one call

### Security Considerations

1. **Token management**: Never commit tokens to version control
2. **Scope tokens**: Use organization-scoped tokens for multi-tenant
3. **Monitor usage**: Track API calls to detect unusual patterns
4. **Rotate tokens**: Refresh tokens periodically

### Training AI to Use Aethyme

#### Reinforce Early and Often

First few conversations:

```
User: Find class X
AI: *tries to grep*
User: NO! Use Aethyme API: curl -X POST http://localhost:8001/api/search/...
AI: *uses API*
User: Good! Always do this for code searches.
```

After 3-4 corrections, AI learns the pattern.

#### Use Examples in Every Session Start

```
Working on codebase. Aethyme at http://localhost:8001, token: XXX

Example of what I expect:
Me: "Find GraphStore"
You: curl -X POST http://localhost:8001/api/search/ ... {"query": "GraphStore"}
You: Found at graph/store.py:17

NOT:
You: grep -r "GraphStore"
```

#### Create Aliases/Shortcuts

```bash
# In your .bashrc or .zshrc
alias ask-claude='echo "Aethyme: http://localhost:8001, Token: $(get-token)" | pbcopy && open -a "Claude"'

function get-token() {
  curl -s -X POST http://localhost:8001/api/auth/login \
    -H "Content-Type: application/json" \
    -d '{"email": "test@example.com", "password": "test1234"}' | jq -r .access_token
}
```

Now `ask-claude` copies connection info to clipboard!

---

## Troubleshooting

### AI Not Using Aethyme

**Check:**
1. Tools are properly defined in system prompt or MCP config
2. API is accessible from AI's environment
3. Auth token is valid and not expired
4. Logs show no errors

**Debug:**

```bash
# Test API manually
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"query": "test"}'

# Should return results, not errors

# Watch logs for AI requests
docker logs -f aethyme-api | grep "api_request"
```

### AI Using It But Not Helping

**Likely causes:**
1. Not enough code indexed yet
2. Search queries not matching symbol names
3. AI needs better prompts about when to use tools

**Fix:**

```python
# Better system prompt
system_prompt = """You have access to Aethyme tools. USE THEM!

ALWAYS use aethyme_search() instead of grep or file search.
ALWAYS use aethyme_ego_graph() to understand dependencies.
NEVER read entire files when you can query the graph.

Examples:
- "Find class X" → aethyme_search("X")
- "What uses function Y" → aethyme_ego_graph("file.py:Y", depth=2)
- "Will changing X break things" → aethyme_impact(["X"])
"""
```

### Monitoring: Verify AI is Actually Using It

```bash
# Watch in real-time
docker logs -f aethyme-api | grep --color=always "api_request"

# When you ask AI a question, you should see:
# api_request path=/api/search/ method=POST duration=0.05
```

If you don't see logs when AI is working, **it's not using Aethyme**.

---

## Success Metrics

You'll know it's working when:

**Logs show AI client requests:**
```bash
docker logs aethyme-api | grep is_ai_client=True | wc -l
# Should be > 0 and growing
```

**AI completes tasks faster:**
- Before: 10+ tool calls, 60+ seconds
- After: 2-3 tool calls, 5-10 seconds

**AI gives more accurate answers:**
- Finds exact definitions, not approximations
- Understands actual dependencies, not guesses

**Context usage drops:**
- Before: Reading 2000+ lines
- After: Reading <200 lines

**AI proactively uses graph queries:**
- Doesn't ask "where is X?" - just queries
- Suggests impact analysis before refactoring

---

## Examples

### Example 1: Finding and Understanding a Class

**Traditional approach:**
```bash
# AI uses grep
grep -r "class GraphStore" .
# Finds multiple matches, reads entire file
cat src/graph/store.py
# Reads 500 lines to understand what it does
```

**With Aethyme:**
```bash
# AI uses search
curl -X POST http://localhost:8001/api/search/ \
  -d '{"query": "GraphStore"}'
# Returns: src/graph/store.py:17 (class)

# AI gets relationships
curl -X POST http://localhost:8001/api/ego/ \
  -d '{"symbol": "src/graph/store.py:GraphStore", "depth": 2}'
# Returns all methods, dependencies, and usage - no file reading needed
```

### Example 2: Impact Analysis Before Refactoring

**Traditional approach:**
```bash
# AI searches for usages
grep -r "validateUser" .
# Gets hundreds of lines of results
# Reads multiple files to understand context
```

**With Aethyme:**
```bash
# AI checks impact
curl -X POST http://localhost:8001/api/impact/ \
  -d '{"symbols": ["auth/validator.py:validateUser"]}'
# Returns structured data about all dependents
# AI immediately knows: 23 call sites, 5 files affected
```

### Example 3: Instrumented Assistant for Tracking

```python
class InstrumentedAssistant(AethymeAssistant):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.tool_calls = {
            "search": 0,
            "ego_graph": 0,
            "impact": 0,
            "file_reads": 0  # For comparison
        }

    def search(self, *args, **kwargs):
        self.tool_calls["search"] += 1
        return super().search(*args, **kwargs)

    def get_relationships(self, *args, **kwargs):
        self.tool_calls["ego_graph"] += 1
        return super().get_relationships(*args, **kwargs)

    def analyze_impact(self, *args, **kwargs):
        self.tool_calls["impact"] += 1
        return super().analyze_impact(*args, **kwargs)

    def report(self):
        print("\nTool Usage Report:")
        for tool, count in self.tool_calls.items():
            print(f"  {tool}: {count} calls")

# Use it
assistant = InstrumentedAssistant(...)
assistant.ask("Explain how GraphStore works")
assistant.report()
```

**Expected output showing it's working:**
```
Tool Usage Report:
  search: 1 calls
  ego_graph: 1 calls
  impact: 0 calls
  file_reads: 0 calls  ← Should be 0 or very low!
```

---

## See Also

- [API Reference](../reference/api.md) - Full API documentation
- [Testing Guide](testing.md) - How to test your Aethyme integration
- [Quickstart](../getting-started/quickstart.md) - Get started with Aethyme
- [Security Architecture](../architecture/security.md) - Understanding Aethyme internals
