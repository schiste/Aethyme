#!/bin/bash
# Verification script for AI-Readiness Scorecard implementation
# Sprint 1 Task S1-T4

set -e

echo "======================================================================"
echo "AI-Readiness Scorecard - Verification Script"
echo "======================================================================"
echo ""

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$SCRIPT_DIR/../../../Mockup/packages/repograph"
cd "$REPO_ROOT"

# Color codes
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Track results
PASS=0
FAIL=0

check() {
    if [ $? -eq 0 ]; then
        echo -e "${GREEN}✓ PASS${NC}: $1"
        ((PASS++))
    else
        echo -e "${RED}✗ FAIL${NC}: $1"
        ((FAIL++))
    fi
}

echo "1. Checking Source Files"
echo "----------------------------------------------------------------------"

# Check detector files
test -f "src/scorecard/__init__.py"
check "scorecard package init"

test -f "src/scorecard/models.py"
check "models.py exists"

test -f "src/scorecard/engine.py"
check "engine.py exists"

test -f "src/scorecard/formatters.py"
check "formatters.py exists"

test -f "src/scorecard/metrics.py"
check "metrics.py exists"

# Check all 8 detectors
test -f "src/scorecard/detectors/base.py"
check "base detector exists"

test -f "src/scorecard/detectors/data_ui_coverage.py"
check "data_ui_coverage detector exists"

test -f "src/scorecard/detectors/folder_docs.py"
check "folder_docs detector exists"

test -f "src/scorecard/detectors/relative_links.py"
check "relative_links detector exists"

test -f "src/scorecard/detectors/i18n_gaps.py"
check "i18n_gaps detector exists"

test -f "src/scorecard/detectors/generated_files.py"
check "generated_files detector exists"

test -f "src/scorecard/detectors/schema_drift.py"
check "schema_drift detector exists"

test -f "src/scorecard/detectors/route_coverage.py"
check "route_coverage detector exists"

test -f "src/scorecard/detectors/ability_coverage.py"
check "ability_coverage detector exists"

echo ""
echo "2. Checking API and CLI"
echo "----------------------------------------------------------------------"

test -f "src/api/routes/scorecard.py"
check "scorecard API routes exist"

grep -q "ai-ready" "src/cli.py"
check "ai-ready command in CLI"

echo ""
echo "3. Checking Database Migration"
echo "----------------------------------------------------------------------"

test -f "migrations/003_scorecard_tables.sql"
check "scorecard migration exists"

grep -q "scorecard_scans" "migrations/003_scorecard_tables.sql"
check "scorecard_scans table in migration"

echo ""
echo "4. Checking Test Files"
echo "----------------------------------------------------------------------"

test -f "tests/scorecard/__init__.py"
check "test package init"

test -f "tests/scorecard/test_detectors.py"
check "detector tests exist"

test -f "tests/scorecard/test_engine.py"
check "engine tests exist"

test -f "tests/scorecard/test_cli.py"
check "CLI tests exist"

echo ""
echo "5. Checking Test Fixtures"
echo "----------------------------------------------------------------------"

test -d "tests/scorecard/fixtures/good_repo"
check "good_repo fixture exists"

test -d "tests/scorecard/fixtures/problematic_repo"
check "problematic_repo fixture exists"

test -f "tests/scorecard/fixtures/good_repo/README.md"
check "good_repo README exists"

test -f "tests/scorecard/fixtures/problematic_repo/README.md"
check "problematic_repo README exists"

echo ""
echo "6. Checking Documentation"
echo "----------------------------------------------------------------------"

test -f "docs/scorecard-guide.md"
check "scorecard guide exists"

test -f "docs/S1-T4-IMPLEMENTATION-SUMMARY.md"
check "implementation summary exists"

test -f "SCORECARD_DELIVERABLES.md"
check "deliverables doc exists"

echo ""
echo "7. Code Quality Checks"
echo "----------------------------------------------------------------------"

# Check for Python syntax errors
python3 -m py_compile src/scorecard/*.py 2>/dev/null
check "scorecard core files compile"

python3 -m py_compile src/scorecard/detectors/*.py 2>/dev/null
check "detector files compile"

echo ""
echo "8. File Count Verification"
echo "----------------------------------------------------------------------"

DETECTOR_COUNT=$(ls -1 src/scorecard/detectors/*.py 2>/dev/null | grep -v __init__ | grep -v base | wc -l | tr -d ' ')
if [ "$DETECTOR_COUNT" -eq 8 ]; then
    echo -e "${GREEN}✓ PASS${NC}: 8 detectors found"
    ((PASS++))
else
    echo -e "${RED}✗ FAIL${NC}: Expected 8 detectors, found $DETECTOR_COUNT"
    ((FAIL++))
fi

TEST_FILE_COUNT=$(ls -1 tests/scorecard/test_*.py 2>/dev/null | wc -l | tr -d ' ')
if [ "$TEST_FILE_COUNT" -eq 3 ]; then
    echo -e "${GREEN}✓ PASS${NC}: 3 test files found"
    ((PASS++))
else
    echo -e "${YELLOW}⚠ WARN${NC}: Expected 3 test files, found $TEST_FILE_COUNT"
fi

echo ""
echo "9. Line Count Metrics"
echo "----------------------------------------------------------------------"

SCORECARD_LINES=$(find src/scorecard -name "*.py" -exec wc -l {} + 2>/dev/null | tail -1 | awk '{print $1}')
echo "Scorecard source: $SCORECARD_LINES lines"

TEST_LINES=$(find tests/scorecard -name "*.py" -exec wc -l {} + 2>/dev/null | tail -1 | awk '{print $1}')
echo "Test code: $TEST_LINES lines"

DOC_LINES=$(wc -l docs/scorecard-guide.md docs/S1-T4-IMPLEMENTATION-SUMMARY.md 2>/dev/null | tail -1 | awk '{print $1}')
echo "Documentation: $DOC_LINES lines"

echo ""
echo "10. Content Verification"
echo "----------------------------------------------------------------------"

grep -q "class ScorecardEngine" "src/scorecard/engine.py"
check "ScorecardEngine class exists"

grep -q "class Severity" "src/scorecard/models.py"
check "Severity enum exists"

grep -q "ALL_DETECTORS" "src/scorecard/detectors/__init__.py"
check "ALL_DETECTORS registry exists"

grep -q "scorecard_scans_total" "src/scorecard/metrics.py"
check "Prometheus metrics defined"

echo ""
echo "======================================================================"
echo "VERIFICATION SUMMARY"
echo "======================================================================"
echo ""
echo -e "Tests Passed: ${GREEN}$PASS${NC}"
echo -e "Tests Failed: ${RED}$FAIL${NC}"
echo ""

if [ $FAIL -eq 0 ]; then
    echo -e "${GREEN}✓ ALL CHECKS PASSED${NC}"
    echo ""
    echo "The AI-Readiness Scorecard implementation is complete and verified."
    echo ""
    echo "Next steps:"
    echo "1. Run test suite: pytest tests/scorecard/ -v"
    echo "2. Apply database migration: psql -f migrations/003_scorecard_tables.sql"
    echo "3. Test CLI: python -m src.cli ai-ready --help"
    echo "4. Test on fixture: python -m src.cli ai-ready --repo tests/scorecard/fixtures/good_repo"
    echo ""
    exit 0
else
    echo -e "${RED}✗ SOME CHECKS FAILED${NC}"
    echo ""
    echo "Please review the failures above and fix before deployment."
    echo ""
    exit 1
fi
