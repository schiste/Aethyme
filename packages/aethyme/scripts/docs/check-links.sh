#!/bin/bash
# Documentation link checker script
# Validates all internal and external links in markdown files

set -e

DOCS_DIR="${1:-docs}"
BROKEN_LINKS=0
CHECKED_LINKS=0

echo "=== Aethyme Documentation Link Checker ==="
echo "Checking links in: $DOCS_DIR"
echo ""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Find all markdown files
MD_FILES=$(find "$DOCS_DIR" -name "*.md" -type f)

# Check internal links
echo "Checking internal links..."
for file in $MD_FILES; do
    # Extract markdown links: [text](link)
    links=$(grep -oP '\[([^\]]+)\]\(([^\)]+)\)' "$file" | grep -oP '\(([^\)]+)\)' | tr -d '()' || true)

    for link in $links; do
        # Skip external links (http/https)
        if [[ $link =~ ^https?:// ]]; then
            continue
        fi

        # Skip anchors
        if [[ $link =~ ^# ]]; then
            continue
        fi

        # Resolve relative path
        dir=$(dirname "$file")
        target="$dir/$link"

        # Check if file exists
        if [ ! -f "$target" ]; then
            echo -e "${RED}✗${NC} Broken link in $file: $link"
            ((BROKEN_LINKS++))
        fi

        ((CHECKED_LINKS++))
    done
done

# Check external links (optional, can be slow)
if [ "$2" == "--check-external" ]; then
    echo ""
    echo "Checking external links..."

    for file in $MD_FILES; do
        # Extract HTTP/HTTPS links
        links=$(grep -oP 'https?://[^\s\)]+' "$file" || true)

        for link in $links; do
            # Check HTTP status
            status=$(curl -s -o /dev/null -w "%{http_code}" -L "$link" --max-time 5 || echo "000")

            if [ "$status" != "200" ] && [ "$status" != "000" ]; then
                echo -e "${YELLOW}⚠${NC} External link status $status in $file: $link"
            fi

            ((CHECKED_LINKS++))
        done
    done
fi

# Summary
echo ""
echo "=== Summary ==="
echo "Total links checked: $CHECKED_LINKS"
echo "Broken internal links: $BROKEN_LINKS"

if [ $BROKEN_LINKS -eq 0 ]; then
    echo -e "${GREEN}✓${NC} All links are valid!"
    exit 0
else
    echo -e "${RED}✗${NC} Found $BROKEN_LINKS broken links"
    exit 1
fi
