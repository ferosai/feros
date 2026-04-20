#!/usr/bin/env bash
set -e

if [ $# -lt 2 ]; then
    echo "Usage: ./release.sh <command> <version>"
    echo "Commands:"
    echo "  prep    - Bump versions, commit, and open a PR"
    echo "  publish - Create a git tag and publish a GitHub release"
    exit 1
fi

COMMAND=$1
NEW_VERSION=$2

if [ "$COMMAND" = "prep" ]; then
    echo "Branching to release/$NEW_VERSION..."
    git checkout -b "release/$NEW_VERSION"
    
    # Extract current version
    OLD_VERSION=$(grep '"version"' studio/web/package.json | head -n 1 | awk -F '"' '{print $4}')
    if [ -z "$OLD_VERSION" ]; then
        echo "Could not find current version in studio/web/package.json"
        exit 1
    fi
    
    echo "Bumping version from $OLD_VERSION to $NEW_VERSION..."
    
    # Cross-platform in-place sed function
    sedi() {
        if [ "$(uname)" = "Darwin" ]; then
            sed -i '' "$@"
        else
            sed -i "$@"
        fi
    }

    find . -type d \( -name node_modules -o -name target -o -name venv -o -name .git -o -name \.next \) -prune -o -type f \( -name "Cargo.toml" -o -name "pyproject.toml" \) -print | while read -r file; do
        sedi -e "s/version = \"$OLD_VERSION\"/version = \"$NEW_VERSION\"/g" "$file"
    done
    
    find . -type d \( -name node_modules -o -name target -o -name venv -o -name .git -o -name \.next \) -prune -o -type f -name "package.json" -print | while read -r file; do
        sedi -e "s/\"version\": \"$OLD_VERSION\"/\"version\": \"$NEW_VERSION\"/g" "$file"
    done
    
    # Run lock updates
    echo "Updating lock files..."
    for d in voice/engine integrations studio/api; do
        if [ -f "$d/pyproject.toml" ]; then
            (cd "$d" && uv lock >/dev/null)
        fi
    done
    
    if [ -d "studio/web" ]; then
        (cd studio/web && pnpm install --lockfile-only --silent)
    fi
    
    for d in voice/engine voice/server integrations; do
        if [ -f "$d/Cargo.toml" ]; then
            (cd "$d" && cargo check -q)
        fi
    done
    
    # Commit and Push
    echo "Committing changes..."
    git add .
    git commit -m "chore: bump version to $NEW_VERSION"
    git push -u origin "release/$NEW_VERSION"
    
    # Create PR via GitHub CLI
    echo "Creating Pull Request..."
    gh pr create \
        --title "Release v$NEW_VERSION" \
        --body "Automated release PR for version \`$NEW_VERSION\`." \
        --base main
        
    echo "--------------------------------------------------------"
    echo "✅ PR Created Successfully!"
    echo "1. Review and land the PR on GitHub."
    echo "2. Check out the main branch (git checkout main && git pull)."
    echo "3. Run 'make release-publish VERSION=$NEW_VERSION'"
    echo "--------------------------------------------------------"

elif [ "$COMMAND" = "publish" ]; then
    echo "Creating git tag v$NEW_VERSION..."
    git tag "v$NEW_VERSION"
    git push origin "refs/tags/v$NEW_VERSION"
    
    echo "Creating GitHub Release..."
    gh release create "v$NEW_VERSION" \
        --title "Release v$NEW_VERSION" \
        --generate-notes
        
    echo "✅ Release v$NEW_VERSION published on GitHub!"
else
    echo "Unknown command: $COMMAND"
    exit 1
fi
