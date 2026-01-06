"""GraphQL API."""

from fastapi import APIRouter
from strawberry.fastapi import GraphQLRouter
import strawberry

# Placeholder GraphQL schema
@strawberry.type
class Query:
    @strawberry.field
    def hello(self) -> str:
        return "Hello from RepoGraph Cloud GraphQL!"

schema = strawberry.Schema(query=Query)

graphql_router = GraphQLRouter(schema)
