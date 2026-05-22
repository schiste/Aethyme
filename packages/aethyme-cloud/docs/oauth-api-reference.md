# OAuth API Reference

**Aethyme Cloud OAuth Integration**

Quick reference for OAuth 2.0 endpoints supporting GitHub, GitLab, and Bitbucket.

---

## Base URL

```
http://localhost:8000/api/oauth
```

Production example: `https://api.example.com/api/oauth`

---

## Authentication

All endpoints require Bearer token authentication:

```bash
Authorization: Bearer {access_token}
```

---

## Endpoints

### 1. List Available Providers

**GET** `/oauth/providers`

Returns list of configured OAuth providers.

**Response:**
```json
{
  "providers": ["github", "gitlab", "bitbucket"],
  "configured": {
    "github": true,
    "gitlab": true,
    "bitbucket": false
  }
}
```

---

### 2. Start OAuth Flow

**GET** `/oauth/{provider}/authorize`

Initiates OAuth authorization flow.

**Parameters:**
- `provider` (path): `github` | `gitlab` | `bitbucket`

**Response:**
```json
{
  "authorization_url": "https://github.com/login/oauth/authorize?client_id=...",
  "state": "random_csrf_token"
}
```

**Frontend Flow:**
1. Call this endpoint
2. Store `state` in sessionStorage
3. Redirect user to `authorization_url`
4. User authorizes on provider's site
5. Provider redirects to callback URL with `code` and `state`

---

### 3. Handle OAuth Callback

**POST** `/oauth/callback`

Exchanges authorization code for access token.

**Request Body:**
```json
{
  "code": "authorization_code_from_provider",
  "state": "csrf_token_from_step_2",
  "provider": "github"
}
```

**Response:**
```json
{
  "provider": "github",
  "provider_user_id": "12345678",
  "username": "johndoe",
  "email": "john@example.com",
  "avatar_url": "https://avatars.githubusercontent.com/u/12345678",
  "access_token": "[ENCRYPTED]"
}
```

**Notes:**
- Access token is encrypted and stored in database
- Returns `[ENCRYPTED]` placeholder for security
- User's GitHub/GitLab/Bitbucket account is now connected

---

### 4. Discover Repositories

**GET** `/oauth/{provider}/repositories`

Lists repositories from connected provider.

**Parameters:**
- `provider` (path): `github` | `gitlab` | `bitbucket`

**Response:**
```json
{
  "repositories": [
    {
      "id": 123456789,
      "name": "my-repo",
      "full_name": "johndoe/my-repo",
      "description": "My awesome repository",
      "private": false,
      "language": "TypeScript",
      "clone_url": "https://github.com/johndoe/my-repo.git",
      "git_url": "git@github.com:johndoe/my-repo.git",
      "default_branch": "main",
      "updated_at": "2025-10-04T12:00:00Z"
    }
  ],
  "total": 1
}
```

**Error Responses:**
- `400` - Provider not connected
- `400` - Invalid or expired token (re-authorize required)
- `500` - Failed to fetch repositories

---

### 5. Connect Repository

**POST** `/oauth/{provider}/repositories/{repo_id}/connect`

Connects a specific repository to Aethyme.

**Parameters:**
- `provider` (path): `github` | `gitlab` | `bitbucket`
- `repo_id` (path): Repository ID from provider

**Response:**
```json
{
  "id": "uuid-here",
  "organization_id": "org-uuid",
  "name": "my-repo",
  "full_name": "johndoe/my-repo",
  "git_url": "git@github.com:johndoe/my-repo.git",
  "clone_url": "https://github.com/johndoe/my-repo.git",
  "provider": "GITHUB",
  "provider_id": "123456789",
  "default_branch": "main",
  "description": "My awesome repository",
  "private": false,
  "language": "TypeScript",
  "status": "pending",
  "created_at": "2025-10-04T12:00:00Z",
  "updated_at": "2025-10-04T12:00:00Z"
}
```

**Notes:**
- Repository status starts as `pending`
- Indexing job will be triggered (Phase 6)
- Webhook will be created automatically (Phase 6)

**Error Responses:**
- `404` - Repository not found
- `400` - Repository already connected

---

### 6. Disconnect Provider

**DELETE** `/oauth/{provider}/disconnect`

Disconnects OAuth provider from user account.

**Parameters:**
- `provider` (path): `github` | `gitlab` | `bitbucket`

**Response:**
```json
{
  "message": "GitHub disconnected successfully"
}
```

**Notes:**
- Removes OAuth tokens from database
- Clears provider ID and username
- Does NOT delete connected repositories
- Repositories will no longer sync until reconnected

---

## Provider-Specific Details

### GitHub

**OAuth Scopes:**
- `repo` - Full repository access
- `user` - User profile information

**API Endpoints Used:**
- Authorization: `https://github.com/login/oauth/authorize`
- Token Exchange: `https://github.com/login/oauth/access_token`
- User Info: `https://api.github.com/user`
- Repositories: `https://api.github.com/user/repos`

**User ID Format:** Numeric (e.g., `12345678`)

---

### GitLab

**OAuth Scopes:**
- `api` - Full API access
- `read_user` - Read user information

**API Endpoints Used:**
- Authorization: `https://gitlab.com/oauth/authorize`
- Token Exchange: `https://gitlab.com/oauth/token`
- User Info: `https://gitlab.com/api/v4/user`
- Repositories: `https://gitlab.com/api/v4/projects?membership=true`

**User ID Format:** Numeric (e.g., `98765`)

---

### Bitbucket

**OAuth Scopes:**
- `repository` - Repository access
- `account` - Account information

**API Endpoints Used:**
- Authorization: `https://bitbucket.org/site/oauth2/authorize`
- Token Exchange: `https://bitbucket.org/site/oauth2/access_token`
- User Info: `https://api.bitbucket.org/2.0/user`
- Repositories: `https://api.bitbucket.org/2.0/repositories?role=member`

**User ID Format:** UUID (e.g., `{12345678-1234-...}`)

---

## Security

### Token Encryption

All OAuth access tokens are encrypted using **Fernet symmetric encryption** before storage:

```python
from cryptography.fernet import Fernet

# Encryption (in OAuth service)
cipher = Fernet(settings.ENCRYPTION_KEY)
encrypted_token = cipher.encrypt(access_token.encode()).decode()

# Decryption (when needed)
decrypted_token = cipher.decrypt(encrypted_token.encode()).decode()
```

### CSRF Protection

State parameter provides CSRF protection:
- Generated with `secrets.token_urlsafe(32)`
- TODO: Stored in Redis with 10-minute expiration
- Validated in callback endpoint

---

## Error Codes

| Code | Description |
|------|-------------|
| 400 | Bad request (invalid provider, missing params) |
| 401 | Unauthorized (invalid or missing token) |
| 404 | Repository not found |
| 500 | Internal server error (API failure, network error) |

---

## Example Flows

### Complete GitHub Connection Flow

```bash
# 1. Start OAuth flow
curl -H "Authorization: Bearer ${TOKEN}" \
  http://localhost:8000/api/oauth/github/authorize

# Returns: {"authorization_url": "...", "state": "..."}

# 2. User clicks authorization_url, authorizes app
# 3. GitHub redirects to: http://localhost:3000/oauth/callback?code=...&state=...

# 4. Frontend exchanges code for token
curl -X POST http://localhost:8000/api/oauth/callback \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{
    "code": "github_authorization_code",
    "state": "csrf_state_from_step_1",
    "provider": "github"
  }'

# Returns: {"provider": "github", "username": "...", ...}

# 5. Discover repositories
curl -H "Authorization: Bearer ${TOKEN}" \
  http://localhost:8000/api/oauth/github/repositories

# Returns: {"repositories": [...], "total": 10}

# 6. Connect a repository
curl -X POST http://localhost:8000/api/oauth/github/repositories/123456789/connect \
  -H "Authorization: Bearer ${TOKEN}"

# Returns: Repository object with status "pending"
```

---

## Frontend Integration

### Using the useOAuth Hook

```tsx
import { useOAuth } from "@/lib/hooks/use-oauth"

function IntegrationsPage() {
  const { connections, connect, disconnect, isLoading } = useOAuth()

  return (
    <div>
      {/* GitHub */}
      <OAuthProviderCard
        provider="github"
        isConnected={connections.github.isConnected}
        username={connections.github.username}
        onConnect={() => connect("github")}
        onDisconnect={() => disconnect("github")}
      />
    </div>
  )
}
```

### Callback Handler

The callback page at `/oauth/callback` automatically:
1. Extracts `code`, `state`, `provider` from URL
2. Calls `/oauth/callback` endpoint
3. Handles success/error states
4. Redirects to `/settings/integrations`

---

## Configuration

### Required Environment Variables

```bash
# Backend (.env)
ENCRYPTION_KEY=your_fernet_key_here

GITHUB_CLIENT_ID=your_github_client_id
GITHUB_CLIENT_SECRET=your_github_client_secret
GITHUB_REDIRECT_URI=http://localhost:3000/oauth/callback?provider=github

GITLAB_CLIENT_ID=your_gitlab_client_id
GITLAB_CLIENT_SECRET=your_gitlab_client_secret
GITLAB_REDIRECT_URI=http://localhost:3000/oauth/callback?provider=gitlab

BITBUCKET_CLIENT_ID=your_bitbucket_client_id
BITBUCKET_CLIENT_SECRET=your_bitbucket_client_secret
BITBUCKET_REDIRECT_URI=http://localhost:3000/oauth/callback?provider=bitbucket
```

### Generating Encryption Key

```bash
# Python
python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())"

# Output: Random_Base64_String_Here=
```

---

**Last Updated:** October 4, 2025
**Version:** 1.0.0
**Phase:** 5 (OAuth Integration Complete)
