"""Bug-fix eval setup: plant/reset deterministic bugs and create test files.

Two scenarios are available:

**implication-share** (original):
  Removes ``Action.SHARE`` from ``PERMISSION_IMPLICATIONS[manage]`` in
  ``packages/auth/src/rbac-canonical.ts``.  Test is placed in the same package
  (``packages/auth/src/__tests__/``).

**cross-package** (efficiency-focused):
  Removes ``Action.EXECUTE`` from ``PERMISSION_IMPLICATIONS[manage]``.  Test is
  placed in ``packages/app-shared/src/tests/`` — 4 import hops from the bug.
  The prompt gives only a behavioral symptom, forcing agents to search across
  packages.  Measures navigation efficiency, not fix difficulty.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

RBAC_REL = "packages/auth/src/rbac-canonical.ts"
TEST_REL = "packages/auth/src/__tests__/ability-implications.test.ts"

# The exact line that should be removed to introduce the bug.
# After the bug is planted, PERMISSION_IMPLICATIONS[manage] will no longer
# contain Action.SHARE.
SHARE_LINE = "    Action.SHARE,"

TEST_CONTENT = """\
import { describe, expect, it } from 'vitest'
import { buildAbilityFromCapabilities } from '../ability'
import { getImpliedActions, actionImplies } from '../rbac-canonical'

describe('permission implications', () => {
  it('manage:suppliers grants share permission via ability builder', () => {
    const ability = buildAbilityFromCapabilities(['manage:suppliers'])
    expect(ability.can('share', 'Suppliers')).toBe(true)
  })

  it('manage:suppliers grants all expected permissions', () => {
    const ability = buildAbilityFromCapabilities(['manage:suppliers'])
    expect(ability.can('read', 'Suppliers')).toBe(true)
    expect(ability.can('create', 'Suppliers')).toBe(true)
    expect(ability.can('update', 'Suppliers')).toBe(true)
    expect(ability.can('delete', 'Suppliers')).toBe(true)
    expect(ability.can('share', 'Suppliers')).toBe(true)
    expect(ability.can('export', 'Suppliers')).toBe(true)
  })

  it('getImpliedActions for manage includes share', () => {
    const implied = getImpliedActions('manage')
    expect(implied).toContain('share')
  })

  it('actionImplies correctly checks manage -> share', () => {
    expect(actionImplies('manage', 'share')).toBe(true)
  })
})
"""

# ---------------------------------------------------------------------------
# Cross-package scenario constants
# ---------------------------------------------------------------------------

# The bug: remove READ from EXECUTE's implications.
# Line 81: ``[Action.EXECUTE]: [Action.READ],`` -> ``[Action.EXECUTE]: [],``
# We can't remove from MANAGE implications because CASL treats 'manage' as a
# built-in wildcard that grants all actions, making the bug invisible at the
# ability.can() level.  EXECUTE→READ is testable because CASL has no magic
# for 'execute'.
EXECUTE_IMPLIES_READ_LINE = "  [Action.EXECUTE]: [Action.READ],"
EXECUTE_IMPLIES_NOTHING = "  [Action.EXECUTE]: [],"

CROSS_PACKAGE_TEST_REL = "packages/app-shared/src/tests/execute-read-implications.test.ts"

# This test imports through app-shared's internal re-export chain, NOT directly
# from packages/auth/.  The agent must trace:
#   test → @/features/auth/utils/ability → @/auth/rbac-canonical → packages/auth/src/rbac-canonical.ts
CROSS_PACKAGE_TEST_CONTENT = """\
import { describe, expect, it } from 'vitest'

import { buildAbilityFromCapabilities } from '@/features/auth/utils/ability'
import { canAll, canAny } from '@/features/auth/utils/can'

describe('execute permission implication chain', () => {
  it('execute permission should grant read access on integrations', () => {
    const ability = buildAbilityFromCapabilities(['execute:admin_integrations'])
    expect(ability.can('read', 'Admin.Integrations')).toBe(true)
  })

  it('execute:integrations grants read as implied sub-permission', () => {
    const ability = buildAbilityFromCapabilities(['execute:admin_integrations'])
    expect(ability.can('execute', 'Admin.Integrations')).toBe(true)
    expect(ability.can('read', 'Admin.Integrations')).toBe(true)
  })

  it('canAll returns true for execute with read check', () => {
    const ability = buildAbilityFromCapabilities(['execute:admin_integrations'])
    expect(canAll(ability, ['read:admin_integrations'])).toBe(true)
    expect(canAll(ability, ['execute:admin_integrations', 'read:admin_integrations'])).toBe(true)
  })

  it('canAny resolves execute-implied read across helpers', () => {
    const ability = buildAbilityFromCapabilities(['execute:admin_integrations'])
    expect(canAny(ability, ['read:admin_integrations'])).toBe(true)
    expect(canAny(ability, ['read:admin_integrations', 'create:admin_integrations'])).toBe(true)
  })
})
"""


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def plant_bug(repo_path: Path) -> dict[str, str]:
    """Remove ``Action.SHARE`` from manage implications.

    Returns a dict with ``original_line`` and ``file`` for diagnostics.
    """
    rbac_path = repo_path / RBAC_REL
    if not rbac_path.exists():
        raise FileNotFoundError(f"Expected {rbac_path} — is this a Playground repo?")

    content = rbac_path.read_text(encoding="utf-8")

    # Verify the share line is present (i.e. bug not already planted).
    if SHARE_LINE not in content:
        # Already planted or structure changed.
        return {"file": RBAC_REL, "status": "already_planted"}

    # Remove the Action.SHARE line from the manage implications block.
    # We match the specific line within PERMISSION_IMPLICATIONS[manage].
    buggy = content.replace(SHARE_LINE + "\n", "", 1)
    rbac_path.write_text(buggy, encoding="utf-8")

    return {"file": RBAC_REL, "original_line": SHARE_LINE, "status": "planted"}


def create_test_file(repo_path: Path) -> Path:
    """Write the ability-implications test to the auth package."""
    test_path = repo_path / TEST_REL
    test_path.parent.mkdir(parents=True, exist_ok=True)
    test_path.write_text(TEST_CONTENT, encoding="utf-8")
    return test_path


def reset_bug(repo_path: Path) -> None:
    """Restore ``rbac-canonical.ts`` to its committed (buggy) state."""
    subprocess.run(
        ["git", "checkout", "HEAD", "--", RBAC_REL],
        cwd=str(repo_path),
        check=True,
        capture_output=True,
    )


def verify_setup(repo_path: Path) -> dict[str, object]:
    """Run vitest and return a diagnostic dict.

    Expected result when the bug is correctly planted:
    - ``fix_test_pass``: False (the planted test should fail)
    - ``existing_tests_pass``: True (existing tests are unaffected)
    """
    fix_result = _run_vitest(repo_path, TEST_REL)
    existing_result = _run_vitest(
        repo_path,
        "packages/auth/src/__tests__/rbac-canonical.test.ts",
        "packages/auth/src/__tests__/permissionGrouping.test.ts",
    )
    return {
        "fix_test_pass": fix_result["pass"],
        "fix_test_output": fix_result["output"],
        "existing_tests_pass": existing_result["pass"],
        "existing_tests_output": existing_result["output"],
    }


def run_fix_test(repo_path: Path) -> dict[str, object]:
    """Run only the planted test and return pass/fail + output."""
    return _run_vitest(repo_path, TEST_REL)


def run_regression_tests(repo_path: Path) -> dict[str, object]:
    """Run all auth tests (planted + existing) and return pass/fail + output."""
    return _run_vitest(repo_path, "packages/auth/src/__tests__/")


# ---------------------------------------------------------------------------
# Cross-package scenario public API
# ---------------------------------------------------------------------------


def plant_cross_package_bug(repo_path: Path) -> dict[str, str]:
    """Replace ``[Action.EXECUTE]: [Action.READ]`` with ``[Action.EXECUTE]: []``.

    This removes READ from execute's implications, breaking the implication
    chain without triggering CASL's built-in manage wildcard.

    Returns a dict with ``original_line`` and ``file`` for diagnostics.
    """
    rbac_path = repo_path / RBAC_REL
    if not rbac_path.exists():
        raise FileNotFoundError(f"Expected {rbac_path} — is this a Playground repo?")

    content = rbac_path.read_text(encoding="utf-8")

    if EXECUTE_IMPLIES_READ_LINE not in content:
        return {"file": RBAC_REL, "status": "already_planted"}

    buggy = content.replace(EXECUTE_IMPLIES_READ_LINE, EXECUTE_IMPLIES_NOTHING, 1)
    rbac_path.write_text(buggy, encoding="utf-8")

    return {
        "file": RBAC_REL,
        "original_line": EXECUTE_IMPLIES_READ_LINE,
        "status": "planted",
    }


def create_cross_package_test(repo_path: Path) -> Path:
    """Write the cross-package test to app-shared/src/tests/."""
    test_path = repo_path / CROSS_PACKAGE_TEST_REL
    test_path.parent.mkdir(parents=True, exist_ok=True)
    test_path.write_text(CROSS_PACKAGE_TEST_CONTENT, encoding="utf-8")
    return test_path


def reset_cross_package_bug(repo_path: Path) -> None:
    """Restore ``rbac-canonical.ts`` to its committed state."""
    subprocess.run(
        ["git", "checkout", "HEAD", "--", RBAC_REL],
        cwd=str(repo_path),
        check=True,
        capture_output=True,
    )


def run_cross_package_fix_test(repo_path: Path) -> dict[str, object]:
    """Run only the cross-package planted test."""
    return _run_vitest(repo_path, CROSS_PACKAGE_TEST_REL)


def run_cross_package_regression_tests(repo_path: Path) -> dict[str, object]:
    """Run auth + app-shared tests (both should pass after fix)."""
    return _run_vitest(
        repo_path,
        "packages/auth/src/__tests__/",
        CROSS_PACKAGE_TEST_REL,
    )


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _run_vitest(repo_path: Path, *test_paths: str) -> dict[str, object]:
    """Run vitest for the given test path(s) and return a result dict."""
    cmd = ["npx", "vitest", "run", *test_paths]
    result = subprocess.run(
        cmd,
        cwd=str(repo_path),
        capture_output=True,
        text=True,
        timeout=30,
    )
    output = result.stdout + result.stderr
    passed = result.returncode == 0
    return {"pass": passed, "output": output, "returncode": result.returncode}
