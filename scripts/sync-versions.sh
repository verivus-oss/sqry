#!/bin/bash
# Sync version from Cargo.toml [workspace.package] to downstream docs, package
# metadata, and first-party cargo-vet exemptions.
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

VET_CONFIG="supply-chain/config.toml"
VET_TMP_FILE=""
WORKSPACE_CRATE_NAMES=()
VET_MANAGED_CRATE_NAMES=()

cleanup_temp_files() {
    if [ -n "$VET_TMP_FILE" ]; then
        rm -f "$VET_TMP_FILE"
    fi
}

trap cleanup_temp_files EXIT

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

    if [ "$file" = ".mcp/server.json" ] && ! grep -q "\"identifier\": \"ghcr.io/verivus-oss/sqry-mcp:v${WORKSPACE_VERSION}\"" "$file"; then
        printf "❌ %-50s identifier tag drift → needs v%s\n" "$label" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
        return
    fi

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

    # Replace both bare semver tokens and `v`-prefixed release tags used in OCI identifiers.
    # Keep the match bounded so unrelated substrings are not rewritten.
    local old_ver_escaped="${old_ver//./\\.}"

    sed -i \
        -e "s/\bv${old_ver_escaped}\b/v${WORKSPACE_VERSION}/g" \
        -e "s/\b${old_ver_escaped}\b/${WORKSPACE_VERSION}/g" \
        "$file"
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

load_workspace_crate_names() {
    if [ "${#WORKSPACE_CRATE_NAMES[@]}" -gt 0 ]; then
        return
    fi

    mapfile -t WORKSPACE_CRATE_NAMES < <(
        cargo metadata --no-deps --format-version 1 \
            | jq -r '.packages[] | select(.source == null) | .name' \
            | sort -u
    )
}

crate_has_vet_policy() {
    local crate="$1"

    awk -v crate="$crate" '
        BEGIN {
            target = "[policy." crate "]"
            in_block = 0
            enabled = 0
        }
        $0 == target {
            in_block = 1
            next
        }
        in_block && /^\[/ {
            exit enabled ? 0 : 1
        }
        in_block && $1 == "audit-as-crates-io" && $3 == "true" {
            enabled = 1
            exit 0
        }
        END {
            exit enabled ? 0 : 1
        }
    ' "$VET_CONFIG"
}

load_vet_managed_crates() {
    if [ "${#VET_MANAGED_CRATE_NAMES[@]}" -gt 0 ]; then
        return
    fi

    if [ ! -f "$VET_CONFIG" ]; then
        return
    fi

    load_workspace_crate_names
    local crate
    for crate in "${WORKSPACE_CRATE_NAMES[@]}"; do
        if crate_has_vet_policy "$crate"; then
            VET_MANAGED_CRATE_NAMES+=("$crate")
        fi
    done
}

get_vet_exemption_versions() {
    local crate="$1"

    awk -v crate="$crate" '
        BEGIN {
            target = "[[exemptions." crate "]]"
            in_block = 0
        }
        $0 == target {
            in_block = 1
            next
        }
        in_block && $1 == "version" {
            gsub(/"/, "", $3)
            print $3
            in_block = 0
            next
        }
        in_block && /^\[\[/ {
            in_block = 0
        }
    ' "$VET_CONFIG"
}

check_vet_exemption_for_crate() {
    local crate="$1"
    local label="cargo-vet exemption: ${crate}"

    if [ ! -f "$VET_CONFIG" ]; then
        printf "⚠️  %-50s config file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    local current_versions
    current_versions=$(get_vet_exemption_versions "$crate")

    if [ -z "$current_versions" ]; then
        printf "❌ %-50s missing → needs %s\n" "$label" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
        return
    fi

    if printf '%s\n' "$current_versions" | grep -qxF "$WORKSPACE_VERSION" \
        && [ "$(printf '%s\n' "$current_versions" | sort -u | wc -l)" -eq 1 ]; then
        printf "✅ %-50s %s\n" "$label" "$WORKSPACE_VERSION"
    else
        local summarized_versions
        summarized_versions=$(printf '%s\n' "$current_versions" | sort -u | paste -sd ',' -)
        printf "❌ %-50s %s → needs %s\n" "$label" "$summarized_versions" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
    fi
}

fix_vet_exemption_for_crate() {
    local crate="$1"
    VET_TMP_FILE="${VET_CONFIG}.tmp"

    awk -v crate="$crate" -v new_ver="$WORKSPACE_VERSION" '
        BEGIN {
            target = "[[exemptions." crate "]]"
            in_block = 0
        }
        $0 == target {
            in_block = 1
            print
            next
        }
        in_block && $1 == "version" {
            print "version = \"" new_ver "\""
            in_block = 0
            next
        }
        in_block && /^\[\[/ {
            in_block = 0
        }
        {
            print
        }
    ' "$VET_CONFIG" > "$VET_TMP_FILE"

    mv "$VET_TMP_FILE" "$VET_CONFIG"
    VET_TMP_FILE=""
    FIXED=$((FIXED + 1))
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

check_date_only_file() {
    local file="$1"
    local label="${2:-$file}"

    if [ ! -f "$file" ]; then
        printf "⚠️  %-50s file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    if grep -q '\*\*Last Updated\*\*:[[:space:]]*[0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}' "$file"; then
        printf "✅ %-50s Last Updated found\n" "$label"
    else
        printf "⚠️  %-50s missing **Last Updated** marker\n" "$label"
        WARNINGS=$((WARNINGS + 1))
    fi
}

# --- Version-only files (Category A) ---
VERSION_ONLY_FILES=(
    "QUICKSTART.md"
    "sqry-cli/README.md"
    ".claude/skills/sqry-semantic-search/SKILL.md"
    ".mcp/server.json"
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

# --- Date-only files (no version field expected) ---
DATE_ONLY_FILES=(
    "docs/templates/README.md"
)

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
echo "--- cargo-vet first-party exemptions ---"
load_vet_managed_crates
for crate in "${VET_MANAGED_CRATE_NAMES[@]}"; do
    check_vet_exemption_for_crate "$crate"
done

echo ""
echo "--- Date-only files ---"
for f in "${DATE_ONLY_FILES[@]}"; do
    check_date_only_file "$f"
done

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

    # Fix first-party cargo-vet exemption drift without touching third-party entries.
    if [ -f "$VET_CONFIG" ]; then
        for crate in "${VET_MANAGED_CRATE_NAMES[@]}"; do
            current_vet_versions=$(get_vet_exemption_versions "$crate")
            if [ -z "$current_vet_versions" ]; then
                printf "⚠️  %-50s missing block; use cargo vet certify or add the exemption manually\n" "cargo-vet exemption: ${crate}"
                WARNINGS=$((WARNINGS + 1))
                continue
            fi

            if ! printf '%s\n' "$current_vet_versions" | grep -qxF "$WORKSPACE_VERSION" \
                || [ "$(printf '%s\n' "$current_vet_versions" | sort -u | wc -l)" -ne 1 ]; then
                current_vet_summary=$(printf '%s\n' "$current_vet_versions" | sort -u | paste -sd ',' -)
                fix_vet_exemption_for_crate "$crate"
                printf "🔧 %-50s %s → %s\n" "cargo-vet exemption: ${crate}" "$current_vet_summary" "$WORKSPACE_VERSION"
            fi
        done
    fi

    # Refresh dates in date-only files
    for f in "${DATE_ONLY_FILES[@]}"; do
        if [ -f "$f" ]; then
            fix_dates_in_file "$f"
        fi
    done

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
    for crate in "${VET_MANAGED_CRATE_NAMES[@]}"; do
        check_vet_exemption_for_crate "$crate"
    done
    for f in "${DATE_ONLY_FILES[@]}"; do
        check_date_only_file "$f"
    done
    for f in "${CHANGELOG_FILES[@]}"; do
        check_changelog "$f"
    done
fi

# --- Summary ---
echo ""
if [ $ERRORS -gt 0 ]; then
    echo "$ERRORS version-managed item(s) need update."
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
