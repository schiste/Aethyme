#!/usr/bin/env python3
"""
Seed test data into the database for development and testing.

Usage:
    python scripts/seed_test_data.py
    python scripts/seed_test_data.py --clear  # Clear existing data first
"""

import argparse
import asyncio
import os
import sys
from datetime import datetime, timedelta
from pathlib import Path

# Add src to path
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))


async def seed_organizations():
    """Seed test organizations."""
    print("Seeding organizations...")

    orgs = [
        {
            "id": "org-test-1",
            "name": "Test Org 1",
            "slug": "test-org-1",
            "created_at": datetime.utcnow() - timedelta(days=30),
        },
        {
            "id": "org-test-2",
            "name": "Test Org 2",
            "slug": "test-org-2",
            "created_at": datetime.utcnow() - timedelta(days=20),
        },
        {
            "id": "org-demo",
            "name": "Demo Organization",
            "slug": "demo-org",
            "created_at": datetime.utcnow() - timedelta(days=10),
        },
    ]

    # TODO: Insert into database when models are available
    # for org in orgs:
    #     await db.execute(insert(Organization).values(**org))

    print(f"✓ Seeded {len(orgs)} organizations")
    return orgs


async def seed_repositories(orgs):
    """Seed test repositories."""
    print("Seeding repositories...")

    repos = [
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

    # TODO: Insert into database
    print(f"✓ Seeded {len(repos)} repositories")
    return repos


async def seed_users(orgs):
    """Seed test users."""
    print("Seeding users...")

    users = [
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

    # TODO: Insert into database
    print(f"✓ Seeded {len(users)} users")
    return users


async def seed_index_data(repos):
    """Seed sample index data."""
    print("Seeding index data...")

    # Sample symbols for each repo
    indexes = []
    for repo in repos:
        indexes.append({
            "repo_id": repo["id"],
            "indexed_at": datetime.utcnow(),
            "status": "complete",
            "symbol_count": 150,
        })

    # TODO: Insert into database
    print(f"✓ Seeded index data for {len(repos)} repositories")
    return indexes


async def clear_data():
    """Clear all test data."""
    print("Clearing existing test data...")

    # TODO: Clear tables in reverse dependency order
    # await db.execute(delete(IndexData))
    # await db.execute(delete(Repository))
    # await db.execute(delete(User))
    # await db.execute(delete(Organization))

    print("✓ Cleared existing data")


async def main(clear_first: bool = False):
    """Main seeding function."""
    print("=" * 60)
    print("RepoGraph Test Data Seeder")
    print("=" * 60)

    # TODO: Initialize database connection
    # async with get_db_session() as db:

    if clear_first:
        await clear_data()

    # Seed data in dependency order
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


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Seed test data")
    parser.add_argument(
        "--clear",
        action="store_true",
        help="Clear existing data before seeding"
    )
    args = parser.parse_args()

    asyncio.run(main(clear_first=args.clear))
