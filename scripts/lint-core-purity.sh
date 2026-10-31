#!/usr/bin/env bash
# Enforce Shiki's `sk-core` purity rule (PLAN.md §0 #3 & T02):
# `sk-core` must have no filesystem I/O, no wall/monotonic clock calls, and no runtime env access.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ROOT_DIR="${1:-$REPO_ROOT}"
CORE_SRC="$ROOT_DIR/crates/sk-core/src"

if [ ! -d "$CORE_SRC" ]; then
    echo "ERROR: Expected sk-core source directory '$CORE_SRC' does not exist." >&2
    exit 1
fi

CORE_FILES=()
while IFS= read -r -d '' file; do
    CORE_FILES+=("$file")
done < <(find "$CORE_SRC" -type f -name '*.rs' -print0)

if [ "${#CORE_FILES[@]}" -eq 0 ]; then
    echo "ERROR: No Rust source files found under '$CORE_SRC'." >&2
    exit 1
fi

FORBIDDEN_PATTERNS=(
    'std::fs'
    'use[[:space:]]+std::[{]([^}]*[^[:alnum:]_:])?(fs|env|process)([^[:alnum:]_:]|$)'
    '(^|[^[:alnum:]_])fs::(File|OpenOptions|DirBuilder|read|read_to_string|read_dir|write|copy|rename|remove_file|remove_dir|remove_dir_all|create_dir|create_dir_all|metadata|symlink_metadata|canonicalize|hard_link|set_permissions)'
    'SystemTime::now'
    'Instant::now'
    'std::time::(SystemTime|Instant)'
    'std::env'
    '(^|[^[:alnum:]_])env::(var|var_os|vars|vars_os|set_var|remove_var|current_dir|set_current_dir|current_exe|temp_dir|home_dir|args|args_os|split_paths|join_paths)'
    'std::process'
    'std::io::(stdin|stdout|stderr)'
)

FAILED=0

for pattern in "${FORBIDDEN_PATTERNS[@]}"; do
    MATCHES="$(grep -EnH "$pattern" "${CORE_FILES[@]}" 2>/dev/null || true)"
    if [ -n "$MATCHES" ]; then
        echo "Purity violation: forbidden impure API pattern '$pattern' found in sk-core:" >&2
        echo "$MATCHES" >&2
        FAILED=1
    fi
done

if [ "$FAILED" -ne 0 ]; then
    echo "ERROR: sk-core purity lint failed. Keep sk-core free of I/O, system clock, and runtime env." >&2
    exit 1
fi

echo "Core purity lint passed: sk-core has no I/O, system clock, or runtime env usage."
