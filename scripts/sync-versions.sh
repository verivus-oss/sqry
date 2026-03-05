#!/bin/bash
# Sync version from Cargo.toml [workspace.package] to all downstream files.
#
# Usage:
#   scripts/sync-versions.sh          # Check mode — exit 1 if any file is stale
#   scripts/sync-versions.sh --fix    # Fix mode — update all files in-place
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

FIX_MODE=false
if [ "${1:-}" = "--fix" ]; then
    FIX_MODE=true
fi

# --- Source of truth ---
WORKSPACE_VERSION=$(grep -A5 '\[workspace.package\]' Cargo.toml \
    | grep '^version' | head -1 \
    | sed 's/.*= *"\([^"]*\)".*/\1/')
TODAY=$(date +%Y-%m-%d)

echo "Workspace version: $WORKSPACE_VERSION"
echo "Date: $TODAY"
echo ""

ERRORS=0
WARNINGS=0
FIXED=0

# --- Helpers ---

# Check that the canonical version field in a file matches WORKSPACE_VERSION.
# Uses pattern-specific checks rather than substring grep to avoid false positives.
check_version_in_file() {
    local file="$1"
    local label="${2:-$file}"

    if [ ! -f "$file" ]; then
        printf "⚠️  %-50s file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    local current_ver
    current_ver=$(detect_old_version "$file")

    if [ "$current_ver" = "$WORKSPACE_VERSION" ]; then
        printf "✅ %-50s %s\n" "$label" "$WORKSPACE_VERSION"
    elif [ -z "$current_ver" ]; then
        printf "❌ %-50s unknown → needs %s\n" "$label" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
    else
        printf "❌ %-50s %s → needs %s\n" "$label" "$current_ver" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
    fi
}

# Check package.json: both .version and .binaryVersion must match.
check_package_json() {
    local file="$1"
    local label="${2:-$file}"

    if [ ! -f "$file" ]; then
        printf "⚠️  %-50s file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    local pkg_ver bin_ver
    pkg_ver=$(jq -r '.version' "$file")
    bin_ver=$(jq -r '.binaryVersion // empty' "$file")

    if [ "$pkg_ver" = "$WORKSPACE_VERSION" ] && { [ -z "$bin_ver" ] || [ "$bin_ver" = "$WORKSPACE_VERSION" ]; }; then
        printf "✅ %-50s %s\n" "$label" "$WORKSPACE_VERSION"
    else
        local detail="version=$pkg_ver"
        if [ -n "$bin_ver" ] && [ "$bin_ver" != "$pkg_ver" ]; then
            detail="version=$pkg_ver, binaryVersion=$bin_ver"
        fi
        printf "❌ %-50s %s → needs %s\n" "$label" "$detail" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
    fi
}

fix_version_in_file() {
    local file="$1"
    local old_ver="$2"

    if [ ! -f "$file" ]; then
        return
    fi

    # Replace only version-like occurrences (bounded by non-alphanumeric chars or line boundaries)
    # to avoid corrupting unrelated content.
    sed -i "s/\b${old_ver}\b/${WORKSPACE_VERSION}/g" "$file"
    FIXED=$((FIXED + 1))
}

fix_dates_in_file() {
    local file="$1"

    if [ ! -f "$file" ]; then
        return
    fi

    # Replace **Last Updated**: YYYY-MM-DD patterns
    sed -i "s/\(\*\*Last Updated\*\*:\s*\)[0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}/\1${TODAY}/g" "$file"
}

detect_old_version() {
    local file="$1"
    local old_ver

    # Try markdown **Version**: pattern first (includes **MCP Server Version**: etc.)
    old_ver=$(grep -oP '\*\*(?:[A-Za-z ]*)?Version\*\*:\s*\K[0-9]+\.[0-9]+\.[0-9]+' "$file" 2>/dev/null | head -1 || true)

    # Try YAML version: pattern
    if [ -z "$old_ver" ]; then
        old_ver=$(grep -oP '^version:\s*\K[0-9]+\.[0-9]+\.[0-9]+' "$file" 2>/dev/null | head -1 || true)
    fi

    # Try JSON "version": pattern
    if [ -z "$old_ver" ]; then
        old_ver=$(grep -oP '"version":\s*"\K[0-9]+\.[0-9]+\.[0-9]+' "$file" 2>/dev/null | head -1 || true)
    fi

    echo "$old_ver"
}

check_changelog() {
    local file="$1"
    local label="${2:-$file}"

    if [ ! -f "$file" ]; then
        printf "⚠️  %-50s file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    if grep -q "## \[${WORKSPACE_VERSION}\]" "$file"; then
        printf "✅ %-50s ## [%s] entry exists\n" "$label" "$WORKSPACE_VERSION"
    else
        printf "⚠️  %-50s missing ## [%s] entry\n" "$label" "$WORKSPACE_VERSION"
        WARNINGS=$((WARNINGS + 1))
    fi
}

# --- Version-only files (Category A) ---
VERSION_ONLY_FILES=(
    "QUICKSTART.md"
    "sqry-cli/README.md"
    ".claude/skills/sqry-semantic-search/SKILL.md"
)

# --- Version + date files (Category B) ---
VERSION_DATE_FILES=(
    "sqry-mcp/README.md"
    "sqry-mcp/USER_GUIDE.md"
    "sqry-mcp/TROUBLESHOOTING.md"
    "tools/sqry-vscode/README.md"
    "tools/sqry-vscode/USER_GUIDE.md"
    "tools/sqry-vscode/TROUBLESHOOTING.md"
    "docs/SCHEMA.md"
    "docs/SEMANTIC_VERSIONING.md"
)

# --- JSON file (special handling) ---
VSCODE_JSON="tools/sqry-vscode/package.json"

# --- Changelog files (check-only) ---
CHANGELOG_FILES=(
    "CHANGELOG.md"
    "tools/sqry-vscode/CHANGELOG.md"
)

echo "--- Version-only files ---"
for f in "${VERSION_ONLY_FILES[@]}"; do
    check_version_in_file "$f"
done

echo ""
echo "--- Version + date files ---"
for f in "${VERSION_DATE_FILES[@]}"; do
    check_version_in_file "$f"
done

echo ""
echo "--- JSON files ---"
check_package_json "$VSCODE_JSON"

echo ""
echo "--- Changelogs (check-only) ---"
for f in "${CHANGELOG_FILES[@]}"; do
    check_changelog "$f"
done

# --- Fix mode ---
if $FIX_MODE && [ $ERRORS -gt 0 ]; then
    echo ""
    echo "--- Fixing versions ---"

    # Fix version-only files
    for f in "${VERSION_ONLY_FILES[@]}"; do
        if [ -f "$f" ]; then
            old_ver=$(detect_old_version "$f")
            if [ -n "$old_ver" ] && [ "$old_ver" != "$WORKSPACE_VERSION" ]; then
                fix_version_in_file "$f" "$old_ver"
                printf "🔧 %-50s %s → %s\n" "$f" "$old_ver" "$WORKSPACE_VERSION"
            fi
        fi
    done

    # Fix version+date files
    for f in "${VERSION_DATE_FILES[@]}"; do
        if [ -f "$f" ]; then
            old_ver=$(detect_old_version "$f")
            if [ -n "$old_ver" ] && [ "$old_ver" != "$WORKSPACE_VERSION" ]; then
                fix_version_in_file "$f" "$old_ver"
                printf "🔧 %-50s %s → %s (version)\n" "$f" "$old_ver" "$WORKSPACE_VERSION"
            fi
            fix_dates_in_file "$f"
        fi
    done

    # Fix package.json with jq — always update both version and binaryVersion
    if [ -f "$VSCODE_JSON" ]; then
        local_ver=$(jq -r '.version' "$VSCODE_JSON")
        local_bin_ver=$(jq -r '.binaryVersion // empty' "$VSCODE_JSON")
        if [ "$local_ver" != "$WORKSPACE_VERSION" ] || { [ -n "$local_bin_ver" ] && [ "$local_bin_ver" != "$WORKSPACE_VERSION" ]; }; then
            jq ".version = \"$WORKSPACE_VERSION\" | .binaryVersion = \"$WORKSPACE_VERSION\"" \
                "$VSCODE_JSON" > "$VSCODE_JSON.tmp"
            mv "$VSCODE_JSON.tmp" "$VSCODE_JSON"
            FIXED=$((FIXED + 1))
            printf "🔧 %-50s → %s\n" "$VSCODE_JSON" "$WORKSPACE_VERSION"
        fi
    fi

    echo ""
    echo "Fixed $FIXED file(s). Re-running check..."
    echo ""

    # Re-check after fix
    ERRORS=0
    WARNINGS=0

    for f in "${VERSION_ONLY_FILES[@]}"; do
        check_version_in_file "$f"
    done
    for f in "${VERSION_DATE_FILES[@]}"; do
        check_version_in_file "$f"
    done
    check_package_json "$VSCODE_JSON"
    for f in "${CHANGELOG_FILES[@]}"; do
        check_changelog "$f"
    done
fi

# --- Summary ---
echo ""
if [ $ERRORS -gt 0 ]; then
    echo "$ERRORS file(s) need version update."
    if [ $WARNINGS -gt 0 ]; then
        echo "$WARNINGS changelog(s) need manual entry."
    fi
    if ! $FIX_MODE; then
        echo "Run with --fix to update versions and dates."
    fi
    exit 1
elif [ $WARNINGS -gt 0 ]; then
    echo "All versions in sync. $WARNINGS changelog(s) need manual entry."
    exit 0
else
    echo "All versions and changelogs in sync."
    exit 0
fi
