# RepoGraph Cloud - Quick Start Guide

**Get up and running in 5 minutes** 🚀

This guide will help you start the RepoGraph Cloud platform locally and test the AI-powered features.

---

## 📋 Prerequisites

- **Node.js** 20+
- **Python** 3.11+
- **Docker** Desktop (for PostgreSQL, Redis, Elasticsearch)
- **pnpm** 8+ (`npm install -g pnpm`)

---

## 🚀 Quick Start (Development)

### Step 1: Start Infrastructure Services

```bash
cd /Users/christophehenner/Downloads/Mockup/packages/repograph-cloud

# Start PostgreSQL, Redis, Elasticsearch
docker-compose up -d

# Verify services are running
docker-compose ps
```

**Expected Output:**
- `repograph-postgres` - PostgreSQL on port 5434
- `repograph-redis` - Redis on port 6381
- `repograph-elasticsearch` - Elasticsearch on port 9202

### Step 2: Set Up Backend (API)

```bash
cd apps/api

# Create virtual environment
python3 -m venv venv
source venv/bin/activate

# Install dependencies
pip install -r requirements.txt

# Set up environment variables
cp .env.example .env

# Generate encryption key for AI credentials
export ENCRYPTION_KEY=$(python3 -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())")
echo "ENCRYPTION_KEY=$ENCRYPTION_KEY" >> .env

# Run database migrations
alembic upgrade head

# Enable pgvector extension (for AI features)
PGPASSWORD=repograph psql -h localhost -p 5434 -U repograph -d repograph_cloud -c "CREATE EXTENSION IF NOT EXISTS vector;"

# Start backend API
uvicorn app.main:app --reload --host 0.0.0.0 --port 8000
```

**Backend will be available at:** http://localhost:8000
**API Docs:** http://localhost:8000/docs

### Step 3: Set Up Frontend (Web)

```bash
# Open new terminal
cd /Users/christophehenner/Downloads/Mockup/packages/repograph-cloud/apps/web

# Install dependencies
pnpm install

# Create environment file
cat > .env.local <<EOF
NEXT_PUBLIC_API_URL=http://localhost:8000
NEXTAUTH_URL=http://localhost:3000
NEXTAUTH_SECRET=$(openssl rand -hex 32)
EOF

# Start frontend dev server
pnpm dev
```

**Frontend will be available at:** http://localhost:3000

### Step 4: (Optional) Start Celery Workers

```bash
# Open new terminal
cd /Users/christophehenner/Downloads/Mockup/packages/repograph-cloud/apps/api
source venv/bin/activate

# Start Celery worker
celery -A app.workers.celery_app worker --loglevel=info
```

---

## 🧪 Testing the Application

### 1. Create an Account

1. Navigate to http://localhost:3000
2. Click "Register"
3. Fill in:
   - Name: Test User
   - Email: test@example.com
   - Password: Password123!
4. Click "Create Account"

### 2. Add AI Credentials (BYOK)

1. **Get an OpenAI API Key:**
   - Go to https://platform.openai.com/api-keys
   - Create a new API key
   - Copy the key (starts with `sk-...`)

2. **Add to RepoGraph:**
   - Navigate to **AI Settings** (left sidebar)
   - Click **"Add Credential"**
   - Select Provider: **OpenAI**
   - Name: "My OpenAI Key"
   - Paste your API key
   - Click **"Add Credential"**
   - Wait for validation ✅

### 3. Connect a Repository

1. Navigate to **Repositories**
2. Click **"Connect Repository"**
3. Choose GitHub (or GitLab/Bitbucket)
4. Authorize the OAuth app
5. Select a repository
6. Wait for indexing to complete (~30 seconds for small repos)

### 4. Generate Embeddings

1. Navigate to **AI Settings** > **Usage & Costs**
2. Click **"Generate Embeddings"** (or wait for automatic generation)
3. Monitor progress in the background jobs

### 5. Try Semantic Search

1. Navigate to **Semantic Search** (left sidebar)
2. Enter a natural language query:
   - "function that validates email addresses"
   - "class for handling database connections"
   - "API endpoint for user authentication"
3. Click **"Search"** or press **⌘+Enter**
4. View results with similarity scores
5. Click **"View"** to jump to code

### 6. Check Usage & Costs

1. Navigate to **AI Settings** > **Usage & Costs**
2. View:
   - Total tokens used
   - Requests made
   - Estimated costs
   - Usage charts (line & pie)
3. Use the **Cost Estimator** to predict future costs

---

## 📊 Verify Everything is Working

### Backend Health Check

```bash
curl http://localhost:8000/health

# Expected response:
# {"status":"healthy","timestamp":"..."}
```

### Frontend Check

```bash
# Open browser
open http://localhost:3000

# You should see the login page
```

### Database Check

```bash
# Check if tables exist
PGPASSWORD=repograph psql -h localhost -p 5434 -U repograph -d repograph_cloud -c "\dt"

# You should see tables like:
# - users
# - organizations
# - repositories
# - api_keys
# - ai_credentials
# - code_embeddings
```

### Elasticsearch Check

```bash
curl http://localhost:9202

# Expected: JSON response with cluster info
```

---

## 🎯 Quick Test Scenarios

### Scenario 1: Full AI Workflow (5 minutes)

1. ✅ Register account
2. ✅ Add OpenAI credential
3. ✅ Connect GitHub repository (public)
4. ✅ Wait for indexing
5. ✅ Generate embeddings (automatic)
6. ✅ Search: "function that parses JSON"
7. ✅ View results
8. ✅ Check usage dashboard

### Scenario 2: Multiple Providers (7 minutes)

1. ✅ Add OpenAI credential
2. ✅ Add Claude credential (Anthropic)
3. ✅ Add Azure OpenAI credential
4. ✅ View all credentials in AI Settings
5. ✅ Revalidate a credential
6. ✅ Delete a credential
7. ✅ Check usage breakdown by provider

### Scenario 3: Cost Estimation (2 minutes)

1. ✅ Navigate to Usage Dashboard
2. ✅ Use Cost Estimator:
   - Symbol count: 50,000
   - Monthly queries: 1,000
   - Provider: OpenAI
   - Model: text-embedding-3-small
3. ✅ View estimated costs
4. ✅ Adjust parameters and see real-time updates

---

## 🐛 Troubleshooting

### Backend won't start

**Error: `ModuleNotFoundError: No module named 'app'`**
```bash
# Make sure you're in the right directory
cd /Users/christophehenner/Downloads/Mockup/packages/repograph-cloud/apps/api

# Make sure venv is activated
source venv/bin/activate

# Reinstall dependencies
pip install -r requirements.txt
```

**Error: `Connection refused` to PostgreSQL**
```bash
# Check if Docker is running
docker ps

# Restart Docker services
docker-compose restart
```

### Frontend won't start

**Error: `ECONNREFUSED` to API**
```bash
# Make sure backend is running on port 8000
curl http://localhost:8000/health

# Check .env.local has correct API URL
cat .env.local | grep API_URL
```

**Error: `Module not found`**
```bash
# Clear .next cache and reinstall
rm -rf .next node_modules
pnpm install
pnpm dev
```

### Celery workers not processing jobs

**Error: `No active workers`**
```bash
# Make sure Redis is running
docker ps | grep redis

# Start worker in verbose mode
celery -A app.workers.celery_app worker --loglevel=debug
```

### Semantic search returns no results

**Check 1: Are embeddings generated?**
```bash
# Check database
PGPASSWORD=repograph psql -h localhost -p 5434 -U repograph -d repograph_cloud -c "SELECT COUNT(*) FROM code_embeddings;"
```

**Check 2: Is pgvector extension installed?**
```bash
PGPASSWORD=repograph psql -h localhost -p 5434 -U repograph -d repograph_cloud -c "\dx"

# Should show "vector" extension
```

**Check 3: Is AI credential valid?**
- Go to AI Settings
- Click "Revalidate" next to your credential
- Check for ✅ Valid status

---

## 🔐 Environment Variables Reference

### Backend (`.env`)

```bash
# Database
DATABASE_URL=postgresql://repograph:repograph@localhost:5434/repograph_cloud

# Redis
REDIS_URL=redis://localhost:6381/0

# Elasticsearch
ELASTICSEARCH_URL=http://localhost:9202

# JWT
JWT_SECRET_KEY=your-secret-key-change-in-production
JWT_ALGORITHM=HS256
ACCESS_TOKEN_EXPIRE_MINUTES=1440

# AI Features
ENCRYPTION_KEY=your-fernet-encryption-key

# CORS
CORS_ORIGINS=http://localhost:3000

# Environment
ENVIRONMENT=development
```

### Frontend (`.env.local`)

```bash
NEXT_PUBLIC_API_URL=http://localhost:8000
NEXTAUTH_URL=http://localhost:3000
NEXTAUTH_SECRET=your-nextauth-secret
```

---

## 📦 Sample Data

### Create Test AI Credential (via API)

```bash
# Get auth token
TOKEN=$(curl -s -X POST http://localhost:8000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email":"test@example.com","password":"Password123!"}' | jq -r .access_token)

# Add OpenAI credential
curl -X POST http://localhost:8000/api/ai/credentials \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "provider_type": "openai",
    "provider_name": "My Test OpenAI Key",
    "credentials": {
      "api_key": "sk-..."
    },
    "validate": true
  }'
```

### Trigger Embedding Generation

```bash
curl -X POST http://localhost:8000/api/semantic/embeddings/generate \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "repository_id": "repo_123",
    "provider_type": "openai",
    "model": "text-embedding-3-small"
  }'
```

### Test Semantic Search

```bash
curl -X POST http://localhost:8000/api/semantic/search \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "function that validates email addresses",
    "limit": 10
  }'
```

---

## 🎉 Success Checklist

- [ ] All 3 Docker services running (PostgreSQL, Redis, Elasticsearch)
- [ ] Backend API responds at http://localhost:8000/health
- [ ] Frontend loads at http://localhost:3000
- [ ] Can register and login
- [ ] Can add AI credential (OpenAI, Claude, or Azure)
- [ ] Credential validates successfully ✅
- [ ] Can navigate to all pages without errors
- [ ] Semantic Search page loads
- [ ] Usage Dashboard displays charts
- [ ] AI Settings page works
- [ ] (Optional) Repository indexing works
- [ ] (Optional) Semantic search returns results

---

## 🚀 Next Steps

Once everything is working locally:

1. **Run Tests**
   ```bash
   # Backend tests
   cd apps/api
   pytest tests/ -v

   # Frontend tests
   cd apps/web
   pnpm test
   ```

2. **Deploy to Staging**
   - See `DEPLOYMENT.md` for production deployment guide

3. **Invite Beta Users**
   - Share http://your-domain.com
   - Collect feedback

---

## 📞 Support

**Documentation:**
- [project-status.md](project-status.md) - Current progress
- [PHASE_12_FRONTEND_COMPLETE.md](PHASE_12_FRONTEND_COMPLETE.md) - Frontend implementation
- [phase-10-11-complete.md](phase-10-11-complete.md) - Backend AI features
- [docs/API_AI_FEATURES.md](docs/API_AI_FEATURES.md) - API reference

**Need Help?**
- Check troubleshooting section above
- Review server logs in terminal
- Check browser console for errors
- Verify all environment variables are set

---

**Last Updated:** October 5, 2025
**Status:** ✅ Ready for local testing
**Estimated Setup Time:** 5-10 minutes
