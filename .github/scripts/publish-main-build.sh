#!/usr/bin/env bash
set -euo pipefail

cd "$1"
tag=main-build
asset="tend-ng-linux-amd64-${GITHUB_SHA:?}.tar.gz"
repo_path="repos/${GITHUB_REPOSITORY:?}"
tag_path="$repo_path/git/ref/tags/$tag"

sha256sum -c "$asset.sha256"
notes=$(mktemp)
trap 'rm -f "$notes"' EXIT

previous=
if release=$(gh release view "$tag" --json assets,isDraft,body); then
    if [[ $(jq -r .isDraft <<< "$release") == false ]]; then
        previous=$(gh api "$tag_path" --jq .object.sha)
        if [[ "$previous" != "$GITHUB_SHA" ]]; then
            relation=$(gh api "$repo_path/compare/$previous...$GITHUB_SHA" --jq .status)
            if [[ "$relation" != ahead ]]; then
                printf 'Keeping the published main build at %s (%s).\n' "$previous" "$relation"
                exit 0
            fi
        fi
    fi
else
    gh release create "$tag" --draft --prerelease --latest=false --target "$GITHUB_SHA" \
        --title "Main build" --notes "Preparing the first main build."
    release='{"assets":[]}'
fi

# Keep the currently advertised archive available while uploading the next one.
# A retry can reuse a complete SHA-named asset uploaded by an earlier attempt.
digest=$(jq -r --arg name "$asset" '
    .assets[] | select(.name == $name and .state == "uploaded") | .digest // empty
' <<< "$release")
if [[ -z "$digest" ]]; then
    gh release upload "$tag" "$asset" --clobber
    digest=$(gh release view "$tag" --json assets | jq -r --arg name "$asset" '
        .assets[] | select(.name == $name and .state == "uploaded") | .digest // empty
    ')
    [[ "$digest" == "sha256:$(cut -d ' ' -f 1 "$asset.sha256")" ]]
fi
[[ "$digest" =~ ^sha256:[a-f0-9]{64}$ ]]

if [[ -n "$previous" && "$previous" != "$GITHUB_SHA" ]]; then
    # GitHub also enforces the forward-only update at the reference.
    gh api -X PATCH "$repo_path/git/refs/tags/$tag" -f sha="$GITHUB_SHA" -F force=false >/dev/null
fi

{
    printf 'Latest successful main build for Linux x86_64 (glibc 2.36).\n\n'
    printf 'Source commit `%s`\n\n' "$GITHUB_SHA"
    printf 'Download [%s](%s/%s/releases/download/%s/%s)\n\n' \
        "$asset" "$GITHUB_SERVER_URL" "$GITHUB_REPOSITORY" "$tag" "$asset"
    printf 'Archive SHA-256 `%s`\n' "${digest#sha256:}"
} > "$notes"
gh release edit "$tag" --draft=false --prerelease --latest=false --target "$GITHUB_SHA" \
    --notes-file "$notes"
