"""GraphQL API."""

import strawberry
from strawberry.fastapi import GraphQLRouter

# Placeholder GraphQL schema
@strawberry.type
class Query:
    @strawberry.field
    def hello(self) -> str:
        return "Hello from Aethyme Cloud GraphQL!"

schema = strawberry.Schema(query=Query)

graphql_router = GraphQLRouter(schema)
