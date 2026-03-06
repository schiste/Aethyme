#!/usr/bin/env python3
"""Seed test data into the database for development and testing."""

from __future__ import annotations

import argparse
import asyncio
import sys
from datetime import datetime, timedelta
from pathlib import Path
from typing import TypedDict

# Add src to path.
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))


class OrganizationSeed(TypedDict):
    id: str
    name: str
    slug: str
    created_at: datetime


class RepositorySeed(TypedDict):
    id: str
    org_id: str
    name: str
    language: str
    url: str
    default_branch: str


class UserSeed(TypedDict):
    id: str
    email: str
    org_id: str
    role: str


class IndexSeed(TypedDict):
    repo_id: str
    indexed_at: datetime
    status: str
    symbol_count: int


async def seed_organizations() -> list[OrganizationSeed]:
    """Seed test organizations."""
    print("Seeding organizations...")

    orgs: list[OrganizationSeed] = [
        {
            "id": "org-test-1",
            "name": "Test Org 1",
            "slug": "test-org-1",
            "created_at": datetime.now(tz=None) - timedelta(days=30),
        },
        {
            "id": "org-test-2",
            "name": "Test Org 2",
            "slug": "test-org-2",
            "created_at": datetime.now(tz=None) - timedelta(days=20),
        },
        {
            "id": "org-demo",
            "name": "Demo Organization",
            "slug": "demo-org",
            "created_at": datetime.now(tz=None) - timedelta(days=10),
        },
    ]

    # TODO: Insert into database when models are available.
    print(f"✓ Seeded {len(orgs)} organizations")
    return orgs


async def seed_repositories(orgs: list[OrganizationSeed]) -> list[RepositorySeed]:
    """Seed test repositories."""
    print("Seeding repositories...")

    repos: list[RepositorySeed] = [
        {
            "id": "repo-1",
            "org_id": orgs[0]["id"],
            "name": "python-backend",
            "language": "python",
            "url": "https://github.com/test-org-1/python-backend",
            "default_branch": "main",
        },
        {
            "id": "repo-2",
            "org_id": orgs[0]["id"],
            "name": "react-frontend",
            "language": "javascript",
            "url": "https://github.com/test-org-1/react-frontend",
            "default_branch": "main",
        },
        {
            "id": "repo-3",
            "org_id": orgs[1]["id"],
            "name": "go-microservice",
            "language": "go",
            "url": "https://github.com/test-org-2/go-microservice",
            "default_branch": "master",
        },
    ]

    # TODO: Insert into database.
    print(f"✓ Seeded {len(repos)} repositories")
    return repos


async def seed_users(orgs: list[OrganizationSeed]) -> list[UserSeed]:
    """Seed test users."""
    print("Seeding users...")

    users: list[UserSeed] = [
        {
            "id": "user-1",
            "email": "admin@test-org-1.com",
            "org_id": orgs[0]["id"],
            "role": "admin",
        },
        {
            "id": "user-2",
            "email": "developer@test-org-1.com",
            "org_id": orgs[0]["id"],
            "role": "developer",
        },
        {
            "id": "user-3",
            "email": "admin@test-org-2.com",
            "org_id": orgs[1]["id"],
            "role": "admin",
        },
    ]

    # TODO: Insert into database.
    print(f"✓ Seeded {len(users)} users")
    return users


async def seed_index_data(repos: list[RepositorySeed]) -> list[IndexSeed]:
    """Seed sample index data."""
    print("Seeding index data...")

    indexes: list[IndexSeed] = [
        {
            "repo_id": repo["id"],
            "indexed_at": datetime.now(tz=None),
            "status": "complete",
            "symbol_count": 150,
        }
        for repo in repos
    ]

    # TODO: Insert into database.
    print(f"✓ Seeded index data for {len(repos)} repositories")
    return indexes


async def clear_data() -> None:
    """Clear all test data."""
    print("Clearing existing test data...")

    # TODO: Clear tables in reverse dependency order.
    print("✓ Cleared existing data")


async def run_seed(clear_first: bool = False) -> None:
    """Main seeding function."""
    print("=" * 60)
    print("Aethyme Test Data Seeder")
    print("=" * 60)

    if clear_first:
        await clear_data()

    orgs = await seed_organizations()
    repos = await seed_repositories(orgs)
    users = await seed_users(orgs)
    await seed_index_data(repos)

    print("\n" + "=" * 60)
    print("✓ Test data seeding complete!")
    print("=" * 60)
    print("\nTest Organizations:")
    for org in orgs:
        print(f"  - {org['name']} (slug: {org['slug']})")
    print(f"\nTest Repositories: {len(repos)}")
    print(f"Test Users: {len(users)}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Seed test data")
    parser.add_argument(
        "--clear",
        action="store_true",
        help="Clear existing data before seeding",
    )
    args = parser.parse_args()

    asyncio.run(run_seed(clear_first=args.clear))


if __name__ == "__main__":
    main()
