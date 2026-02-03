"""API v1 routes."""

from fastapi import APIRouter
from app.api.v1 import (
    auth,
    users,
    organizations,
    repositories,
    api_keys,
    health,
    github,
    jobs,
    search,
    dashboard,
    oauth,
    webhooks,
    ai_credentials,
    semantic_search,
    graph,
    documentation,
    presence,
    activity,
    comments,
    annotations,
)

router = APIRouter()

# Include all v1 routers
router.include_router(health.router, prefix="/health", tags=["health"])
router.include_router(auth.router, prefix="/auth", tags=["auth"])
router.include_router(users.router, prefix="/users", tags=["users"])
router.include_router(organizations.router, prefix="/organizations", tags=["organizations"])
router.include_router(repositories.router, prefix="/repositories", tags=["repositories"])
router.include_router(api_keys.router, prefix="/api-keys", tags=["api-keys"])
router.include_router(github.router, prefix="/auth/github", tags=["github-oauth"])
router.include_router(oauth.router, prefix="/oauth", tags=["oauth"])
router.include_router(jobs.router, prefix="/jobs", tags=["background-jobs"])
router.include_router(search.router, prefix="/search", tags=["code-search"])
router.include_router(dashboard.router, prefix="/dashboard", tags=["dashboard"])
router.include_router(webhooks.router, prefix="/webhooks", tags=["webhooks"])
router.include_router(ai_credentials.router, prefix="/ai", tags=["ai-credentials"])
router.include_router(semantic_search.router, prefix="/semantic", tags=["semantic-search"])
router.include_router(graph.router, prefix="/graph", tags=["graph-analysis"])
router.include_router(documentation.router, prefix="/docs", tags=["documentation"])
router.include_router(presence.router, prefix="/presence", tags=["presence"])
router.include_router(activity.router, prefix="/activity", tags=["activity"])
router.include_router(comments.router, prefix="/comments", tags=["comments"])
router.include_router(annotations.router, prefix="/annotations", tags=["annotations"])
