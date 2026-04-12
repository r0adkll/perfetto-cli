#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# release.sh — bump version, update changelog, commit, tag, push
#
# Usage:
#   ./scripts/release.sh <major|minor|patch>
#   ./scripts/release.sh 1.2.3          # explicit version
#
# Examples:
#   ./scripts/release.sh patch          # 0.1.0 → 0.1.1
#   ./scripts/release.sh minor          # 0.1.0 → 0.2.0
#   ./scripts/release.sh major          # 0.1.0 → 1.0.0
#   ./scripts/release.sh 2.0.0-beta.1   # explicit
# ---------------------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

die() { echo -e "${RED}error:${NC} $1" >&2; exit 1; }
info() { echo -e "${CYAN}▶${NC} $1"; }
ok() { echo -e "${GREEN}✓${NC} $1"; }

# --- Validate args ---------------------------------------------------------

[[ $# -eq 1 ]] || die "usage: release.sh <major|minor|patch|X.Y.Z>"

BUMP="$1"

# --- Read current version from Cargo.toml ----------------------------------

CURRENT=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
[[ -n "$CURRENT" ]] || die "could not read version from Cargo.toml"

IFS='.' read -r CUR_MAJOR CUR_MINOR CUR_PATCH <<< "$CURRENT"

# --- Compute next version ---------------------------------------------------

case "$BUMP" in
    major) NEXT="$((CUR_MAJOR + 1)).0.0" ;;
    minor) NEXT="${CUR_MAJOR}.$((CUR_MINOR + 1)).0" ;;
    patch) NEXT="${CUR_MAJOR}.${CUR_MINOR}.$((CUR_PATCH + 1))" ;;
    *)
        # Validate as semver (X.Y.Z with optional pre-release/build metadata)
        [[ "$BUMP" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?(\+[0-9A-Za-z.]+)?$ ]] \
            || die "invalid argument '${BUMP}' — expected major|minor|patch or a valid semver (e.g. 1.2.3)"
        NEXT="$BUMP"
        ;;
esac

TAG="v${NEXT}"
TODAY=$(date +%Y-%m-%d)

info "Releasing ${YELLOW}${CURRENT}${NC} → ${GREEN}${NEXT}${NC} (${TAG})"

# --- Preflight checks -------------------------------------------------------

[[ -z "$(git status --porcelain)" ]] || die "working tree is dirty — commit or stash first"
git fetch --tags --quiet
git tag -l "$TAG" | grep -q . && die "tag ${TAG} already exists"

# --- Bump version in Cargo.toml --------------------------------------------

sed -i '' "s/^version = \"${CURRENT}\"/version = \"${NEXT}\"/" Cargo.toml
cargo check --quiet 2>/dev/null  # refresh Cargo.lock
ok "Bumped Cargo.toml to ${NEXT}"

# --- Update CHANGELOG.md ---------------------------------------------------

if [[ -f CHANGELOG.md ]]; then
    # Replace the [Unreleased] header with the new version + date,
    # then re-add a fresh [Unreleased] section above it.
    sed -i '' "s/^## \[Unreleased\]/## [Unreleased]\n\n## [${NEXT}] - ${TODAY}/" CHANGELOG.md

    # Update the comparison links at the bottom.
    # Replace the existing Unreleased link to point at the new tag.
    sed -i '' "s|\[Unreleased\]: \(.*\)/compare/v.*\.\.\.HEAD|[Unreleased]: \1/compare/${TAG}...HEAD|" CHANGELOG.md

    # Add the new version link if it doesn't already exist.
    if ! grep -q "^\[${NEXT}\]:" CHANGELOG.md; then
        # Insert before the last version link line
        sed -i '' "/^\[Unreleased\]:/a\\
[${NEXT}]: https://github.com/r0adkll/perfetto-cli/compare/v${CURRENT}...${TAG}" CHANGELOG.md
    fi

    ok "Updated CHANGELOG.md for ${NEXT}"
else
    echo -e "${YELLOW}⚠${NC}  No CHANGELOG.md found — skipping"
fi

# --- Commit, tag, push ------------------------------------------------------

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "Release ${TAG}"
ok "Committed release"

git tag -a "$TAG" -m "Release ${TAG}"
ok "Created tag ${TAG}"

info "Pushing to origin..."
git push origin main "$TAG"
ok "Pushed main + ${TAG}"

echo ""
echo -e "${GREEN}🎉 Release ${TAG} is live!${NC}"
echo -e "   Monitor the build: ${CYAN}https://github.com/r0adkll/perfetto-cli/actions${NC}"
