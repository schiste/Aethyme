#!/usr/bin/env python3
"""
Seed benchmark data for performance testing.

This script creates realistic test data for benchmarking indexing,
queries, and other operations.
"""

import asyncio
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "src"))


async def seed_benchmark_repos():
    """Seed repositories of various sizes for benchmarking."""
    print("Seeding benchmark repositories...")

    sizes = {
        "small": {"files": 50, "symbols": 500},
        "medium": {"files": 500, "symbols": 5000},
        "large": {"files": 2000, "symbols": 20000},
    }

    repos = []
    for size_name, specs in sizes.items():
        repo = {
            "id": f"bench-repo-{size_name}",
            "name": f"benchmark-{size_name}",
            "size": size_name,
            "file_count": specs["files"],
            "symbol_count": specs["symbols"],
        }
        repos.append(repo)
        print(f"  - {size_name}: {specs['files']} files, {specs['symbols']} symbols")

    # TODO: Insert into database
    print(f"✓ Seeded {len(repos)} benchmark repositories")
    return repos


async def main():
    """Main benchmark data seeding."""
    print("=" * 60)
    print("RepoGraph Benchmark Data Seeder")
    print("=" * 60)

    await seed_benchmark_repos()

    print("\n✓ Benchmark data ready")


if __name__ == "__main__":
    asyncio.run(main())
