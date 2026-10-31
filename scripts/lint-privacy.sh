#!/usr/bin/env bash
# Enforce Shiki's hard privacy rule (PLAN.md §0 #4):
# no network crates, network APIs, or CoreLocation anywhere in the codebase.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ROOT_DIR="${1:-$REPO_ROOT}"

if [ ! -d "$ROOT_DIR" ]; then
    echo "ERROR: Target directory '$ROOT_DIR' does not exist." >&2
    exit 1
fi

SOURCE_PATTERNS=(
    '(std|core|tokio|async_std|mio|smol)::net'
    'net::(TcpStream|TcpListener|UdpSocket|SocketAddr|SocketAddrV4|SocketAddrV6|IpAddr|Ipv4Addr|Ipv6Addr|ToSocketAddrs)'
    'use[[:space:]]+(std|core|tokio|async_std|mio|smol)::[{]([^}]*[^[:alnum:]_:])?net([^[:alnum:]_:]|$)'
    '(^|[^[:alnum:]_])(reqwest|hyper|ureq|surf|isahc|attohttpc|minreq|socket2)([^[:alnum:]_]|$)'
    '(^|[^[:alnum:]_])(URLSession|URLSessionConfiguration|URLSessionTask|NWConnection|NWPathMonitor|CFNetwork|CFSocket|CFHTTPMessage)([^[:alnum:]_]|$)'
    '(^|[^[:alnum:]_])(CoreLocation|CLLocation|CLLocationManager|CLGeocoder)([^[:alnum:]_]|$)'
    '(^|[^[:alnum:]_])import[[:space:]]+(Network|CoreLocation)([^[:alnum:]_]|$)'
)

MANIFEST_PATTERNS=(
    '(^|[^[:alnum:]_-])(reqwest|hyper|ureq|surf|isahc|curl|attohttpc|minreq|socket2|rustls|native-tls|tokio-native-tls|tokio-rustls)([^[:alnum:]_-]|$)'
)

SOURCE_FILES=()
MANIFEST_FILES=()

for subdir in crates tests macos; do
    if [ -d "$ROOT_DIR/$subdir" ]; then
        while IFS= read -r -d '' file; do
            SOURCE_FILES+=("$file")
        done < <(find "$ROOT_DIR/$subdir" -type f \( -name '*.rs' -o -name '*.swift' \) -print0)

        while IFS= read -r -d '' file; do
            MANIFEST_FILES+=("$file")
        done < <(find "$ROOT_DIR/$subdir" -type f -name 'Cargo.toml' -print0)
    fi
done

for root_manifest in Cargo.toml Cargo.lock; do
    if [ -f "$ROOT_DIR/$root_manifest" ]; then
        MANIFEST_FILES+=("$ROOT_DIR/$root_manifest")
    fi
done

FAILED=0

if [ "${#SOURCE_FILES[@]}" -gt 0 ]; then
    for pattern in "${SOURCE_PATTERNS[@]}"; do
        MATCHES="$(grep -EnH "$pattern" "${SOURCE_FILES[@]}" 2>/dev/null || true)"
        if [ -n "$MATCHES" ]; then
            echo "Privacy violation: forbidden network/geolocation API or crate pattern '$pattern' found in source:" >&2
            echo "$MATCHES" >&2
            FAILED=1
        fi
    done
fi

if [ "${#MANIFEST_FILES[@]}" -gt 0 ]; then
    for pattern in "${MANIFEST_PATTERNS[@]}"; do
        MATCHES="$(grep -EnH "$pattern" "${MANIFEST_FILES[@]}" 2>/dev/null || true)"
        if [ -n "$MATCHES" ]; then
            echo "Privacy violation: forbidden networking crate pattern '$pattern' found in Cargo manifest/lockfile:" >&2
            echo "$MATCHES" >&2
            FAILED=1
        fi
    done
fi

if [ "$FAILED" -ne 0 ]; then
    echo "ERROR: Privacy lint failed. Shiki forbids all network APIs, networking crates, and CoreLocation." >&2
    exit 1
fi

echo "Privacy lint passed: no network APIs, networking crates, or CoreLocation detected."
