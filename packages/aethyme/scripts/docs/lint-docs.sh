#!/bin/bash
# Documentation linter script
# Checks markdown formatting, spelling, and style

set -e

DOCS_DIR="${1:-docs}"
ERRORS=0

echo "=== Aethyme Documentation Linter ==="
echo "Linting: $DOCS_DIR"
echo ""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Check if markdownlint is installed
if ! command -v markdownlint &> /dev/null; then
    echo -e "${YELLOW}Warning: markdownlint not installed${NC}"
    echo "Install with: npm install -g markdownlint-cli"
    echo "Skipping markdown linting..."
else
    echo "Running markdownlint..."
    if markdownlint "$DOCS_DIR/**/*.md" --config .markdownlint.json 2>&1; then
        echo -e "${GREEN}✓${NC} Markdown formatting OK"
    else
        echo -e "${RED}✗${NC} Markdown formatting errors found"
        ((ERRORS++))
    fi
fi

# Check for common issues
echo ""
echo "Checking for common documentation issues..."

MD_FILES=$(find "$DOCS_DIR" -name "*.md" -type f)

for file in $MD_FILES; do
    # Check for trailing whitespace
    if grep -n '[ \t]$' "$file" > /dev/null; then
        echo -e "${YELLOW}⚠${NC} Trailing whitespace in: $file"
    fi

    # Check for tabs (should use spaces)
    if grep -P '\t' "$file" > /dev/null; then
        echo -e "${YELLOW}⚠${NC} Tabs found in: $file (use spaces)"
    fi

    # Check for TODO/FIXME comments
    if grep -n 'TODO\|FIXME\|XXX' "$file" > /dev/null; then
        echo -e "${YELLOW}⚠${NC} TODO/FIXME found in: $file"
    fi

    # Check for placeholder text
    if grep -n 'lorem ipsum\|TODO:\|PLACEHOLDER' "$file" -i > /dev/null; then
        echo -e "${RED}✗${NC} Placeholder text in: $file"
        ((ERRORS++))
    fi

    # Check for broken image references
    if grep -oP '!\[([^\]]*)\]\(([^\)]+)\)' "$file" > /dev/null; then
        images=$(grep -oP '!\[([^\]]*)\]\(([^\)]+)\)' "$file" | grep -oP '\(([^\)]+)\)' | tr -d '()' || true)
        for img in $images; do
            if [[ ! $img =~ ^https?:// ]]; then
                dir=$(dirname "$file")
                if [ ! -f "$dir/$img" ]; then
                    echo -e "${RED}✗${NC} Broken image in $file: $img"
                    ((ERRORS++))
                fi
            fi
        done
    fi
done

# Check for spelling (if aspell is available)
if command -v aspell &> /dev/null; then
    echo ""
    echo "Running spell check..."
    MISSPELLINGS=0

    for file in $MD_FILES; do
        # Extract text and check spelling
        misspelled=$(cat "$file" | aspell list --lang=en --mode=markdown | sort -u || true)
        if [ -n "$misspelled" ]; then
            echo -e "${YELLOW}⚠${NC} Possible misspellings in $file:"
            echo "$misspelled" | head -10
            ((MISSPELLINGS++))
        fi
    done

    if [ $MISSPELLINGS -eq 0 ]; then
        echo -e "${GREEN}✓${NC} No obvious misspellings"
    fi
else
    echo -e "${YELLOW}Note: aspell not installed, skipping spell check${NC}"
fi

# Summary
echo ""
echo "=== Summary ==="
if [ $ERRORS -eq 0 ]; then
    echo -e "${GREEN}✓${NC} Documentation linting passed!"
    exit 0
else
    echo -e "${RED}✗${NC} Found $ERRORS errors"
    exit 1
fi
