#!/usr/bin/env bash
set -euo pipefail

components=$1
output=$2
name="tend-ng-linux-amd64-${GITHUB_SHA:?}"
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT

mkdir -p "$staging/bin" "$staging/web" "$output"
install -m 755 "$components/rust/"{tend-server,tend-runner,tend} "$staging/bin/"
install -m 755 "$components/tsnet/tend-tsnet" "$staging/bin/"
test -s "$components/web/index.html"
cp -R "$components/web/." "$staging/web/"
printf '%s\n' "$GITHUB_SHA" > "$staging/REVISION"

# Check the deployment baseline against the binaries we actually package.
for binary in "$staging/bin/"*; do
    symbols=$(readelf --version-info "$binary")
    required=$(printf '%s\n' "$symbols" | awk '
        { for (i = 1; i <= NF; i++) if ($i ~ /^GLIBC_[0-9.]+$/) print substr($i, 7) }
    ' | sort -V | tail -n 1)
    if [[ -n "$required" && $(printf '%s\n' 2.36 "$required" | sort -V | tail -n 1) != 2.36 ]]; then
        printf '%s requires glibc %s, beyond the deployment baseline 2.36\n' "$binary" "$required" >&2
        exit 1
    fi
done

install -m 755 scripts/tend-runner-upgrade "$staging/bin/"
test -x "$staging/bin/tend-runner-upgrade"

tar -czf "$output/$name.tar.gz" -C "$staging" bin web REVISION
cd "$output"
sha256sum "$name.tar.gz" > "$name.tar.gz.sha256"
