#!/usr/bin/env bash
# Maintainer utility to refresh vendored IANA tzdb tables (`data/tz/`) from
# https://data.iana.org/time-zones/tzdb/ (`PLAN.md` §4, §8).
#
# Never invoked at runtime by Shiki (`shiki-term` is 100% offline).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TZ_DIR="$REPO_ROOT/data/tz"
BASE_URL="${IANA_TZDB_URL:-https://data.iana.org/time-zones/tzdb}"

mkdir -p "$TZ_DIR"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fsSL "$BASE_URL/version" -o "$TMP_DIR/version"
curl -fsSL "$BASE_URL/zone1970.tab" -o "$TMP_DIR/zone1970.tab"
curl -fsSL "$BASE_URL/backward" -o "$TMP_DIR/backward"

VERSION="$(tr -d '[:space:]' < "$TMP_DIR/version")"
if [ -z "$VERSION" ]; then
    echo "ERROR: Empty tzdb version retrieved from $BASE_URL/version" >&2
    exit 1
fi

if ! grep -q 'Asia/Ho_Chi_Minh' "$TMP_DIR/zone1970.tab"; then
    echo "ERROR: Downloaded zone1970.tab failed sanity check" >&2
    exit 1
fi

if ! grep -q '^Link[[:space:]]\+Asia/Ho_Chi_Minh[[:space:]]\+Asia/Saigon' "$TMP_DIR/backward"; then
    echo "ERROR: Downloaded backward file failed sanity check" >&2
    exit 1
fi

TODAY="$(date -u +%Y-%m-%d)"
cat > "$TMP_DIR/VERSION" <<EOF
# Vendored IANA Time Zone Database metadata for Shiki (\`PLAN.md\` §4, §8, T04)
# Source URL: $BASE_URL/
# Retrieved: $TODAY
# License: Public Domain
$VERSION
EOF

mv "$TMP_DIR/VERSION" "$TZ_DIR/VERSION"
mv "$TMP_DIR/zone1970.tab" "$TZ_DIR/zone1970.tab"
mv "$TMP_DIR/backward" "$TZ_DIR/backward"

echo "Updated vendored IANA tzdb in $TZ_DIR to version $VERSION ($TODAY)."
