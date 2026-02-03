# How AI Agents Discover and Use Aethyme

## The Core Problem: AI Agents Don't Know About Aethyme by Default

When you talk to Claude, GPT-4, or any AI assistant, they have:
- ✅ Built-in tools (file reading, bash commands, web search)
- ❌ NO knowledge that Aethyme exists
- ❌ NO way to access your Aethyme API

**You must explicitly integrate Aethyme into their workflow.**

---

## Integration Methods (How AI Learns About Aethyme)

### Option 1: MCP (Model Context Protocol) - For Claude ⭐ RECOMMENDED

MCP is Claude's native way to add custom tools. When configured, Aethyme appears in Claude Desktop's tool menu automatically.

#### How It Works:

1. **You create an MCP server** that wraps Aethyme API
2. **You configure Claude Desktop** to load this server
3. **Claude automatically discovers** the tools when it starts
4. **Claude knows to use them** because they appear in its tool list with descriptions

#### Setup:

**1. Create MCP configuration file:**

```bash
# Location: ~/.config/claude/mcp_servers.json (Mac/Linux)
# or: %APPDATA%\Claude\mcp_servers.json (Windows)

mkdir -p ~/.config/claude
cat > ~/.config/claude/mcp_servers.json << 'EOF'
{
  "aethyme": {
    "command": "node",
    "args": ["Mockup/packages/aethyme/mcp-server.js"],
    "env": {
      "AETHYME_API_URL": "http://localhost:8001",
      "AETHYME_TOKEN": "your-token-here"
    }
  }
}
EOF
```

**2. Create the MCP server file:**

```bash
cat > packages/aethyme/mcp-server.js << 'EOF'
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
          description: "List of symbols to analyze (e.g., ['graph/store.py:GraphStore', 'models/graph.py:Node'])"
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
EOF

chmod +x packages/aethyme/mcp-server.js
```

**3. Install dependencies:**

```bash
cd packages/aethyme
npm init -y
npm install node-fetch
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

After restart, when you ask me questions, I'll see these tools in my tool list:
- `aethyme_search`
- `aethyme_ego_graph`
- `aethyme_impact_analysis`

**I'll automatically use them** because they appear in my available tools with clear descriptions!

---

### Option 2: System Prompt Instructions - For Any AI

If you can't use MCP (e.g., using ChatGPT, Claude in browser, or API), you can tell the AI about Aethyme in your instructions.

#### How It Works:

You start EVERY conversation with instructions that include:
1. Aethyme API details
2. When to use it
3. Example calls

#### Setup:

**Create a prompt template:**

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

YOUR_TOKEN = eyJhbGci...

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

**Usage:**

Every time you start a conversation with an AI:

```bash
# Get fresh token
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

### Option 3: Custom AI Agent - Full Integration

Build a dedicated AI agent that ALWAYS uses Aethyme.

#### How It Works:

Create a wrapper around the AI that:
1. Intercepts all queries
2. Automatically searches Aethyme first
3. Provides context to AI
4. AI never needs to know about Aethyme directly

#### Setup:

**Create the agent:**

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

**Run it:**

```bash
export ANTHROPIC_API_KEY="your-key"
python packages/aethyme/aethyme-agent.py
```

Now when you ask "Find the GraphStore class", the agent:
1. Detects "GraphStore" in your message
2. Automatically searches Aethyme
3. Provides results to Claude
4. Claude answers with exact information

**Claude doesn't even need to know about the API** - the wrapper handles everything!

---

## Comparison: Which Method Should You Use?

| Method | Setup Effort | User Experience | AI Awareness | Best For |
|--------|-------------|-----------------|--------------|----------|
| **MCP** | Medium (one-time) | ⭐⭐⭐⭐⭐ Seamless | AI knows tools exist | Claude Desktop users |
| **System Prompt** | Low (every session) | ⭐⭐⭐ Manual | AI must remember | Any AI, quick tests |
| **Custom Agent** | High (one-time) | ⭐⭐⭐⭐ Automatic | AI unaware (transparent) | Production, automation |

---

## Quick Test: Verify AI Knows About Aethyme

### Test 1: MCP Installation (Claude Desktop)

```bash
# Check MCP config exists
cat ~/.config/claude/mcp_servers.json

# Expected: Should show aethyme configuration

# Restart Claude Desktop, then in chat say:
# "What tools do you have available?"

# I should mention aethyme_search, aethyme_ego_graph, etc.
```

### Test 2: System Prompt (Any AI)

Start a conversation:

```
I'm using Aethyme at http://localhost:8001
Token: [your-token]

Rules:
- Use POST /api/search/ to find symbols
- Use POST /api/ego/ for relationships

Find the GraphStore class.
```

Watch if AI uses `curl` to call the API instead of `grep`.

### Test 3: Custom Agent

```bash
python aethyme-agent.py

# At prompt, type:
You: Where is the GraphStore class?

# Watch terminal for:
🔍 Auto-searching Aethyme for: GraphStore

# Agent should auto-query before answering
```

---

## Why AI Doesn't Use Aethyme Automatically

**AI models have no built-in knowledge of:**
- Your local development environment
- Running services on localhost
- Custom APIs you've deployed
- Authentication tokens

**They only know about:**
- Tools explicitly defined in their configuration (MCP)
- Instructions you give them in prompts
- Built-in capabilities (file read, bash, web search)

**This is by design** - for security and privacy. AI can't discover or access your services without explicit permission.

---

## Making It Stick: How to Train AI to Use Aethyme

### 1. **Reinforce Early and Often**

First few conversations:

```
User: Find class X
AI: *tries to grep*
User: NO! Use Aethyme API: curl -X POST http://localhost:8001/api/search/...
AI: *uses API*
User: Good! Always do this for code searches.
```

After 3-4 corrections, AI learns the pattern.

### 2. **Use Examples in Every Session Start**

```
Working on codebase. Aethyme at http://localhost:8001, token: XXX

Example of what I expect:
Me: "Find GraphStore"
You: curl -X POST http://localhost:8001/api/search/ ... {"query": "GraphStore"}
You: Found at graph/store.py:17

NOT:
You: grep -r "GraphStore"
```

### 3. **Create Aliases/Shortcuts**

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

## Monitoring: Verify AI is Actually Using It

```bash
# Watch in real-time
docker logs -f aethyme-api | grep --color=always "api_request"

# When you ask AI a question, you should see:
# api_request path=/api/search/ method=POST duration=0.05
```

If you don't see logs when AI is working, **it's not using Aethyme**.

---

## Summary: The Answer to "How Do AI Agents Know?"

**Short answer:** They don't, unless you tell them.

**Ways to tell them:**

1. ✅ **MCP** - Define tools in config (best for Claude Desktop)
2. ✅ **System Prompt** - Explain in every conversation (universal)
3. ✅ **Custom Wrapper** - Build agent that auto-uses it (production)
4. ❌ **Automatic Discovery** - Not possible (by design)

**Recommended approach:**

- **For daily use:** Set up MCP (one-time, seamless)
- **For team use:** Create custom agent or shared prompts
- **For testing:** Use system prompts

The key is **explicit integration** - AI won't discover it on its own, but once configured, it will prefer Aethyme over traditional file operations because it's faster and returns better data.
