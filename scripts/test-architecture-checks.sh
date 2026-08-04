#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

assert_rejected() {
    label=$1
    if (
        cd "$tmp"
        "$root/scripts/check-architecture.sh"
    ) >/dev/null 2>&1; then
        printf '%s\n' "architecture checker failed to reject $label" >&2
        exit 1
    fi
}

reset_fixture() {
    rm -rf "$tmp/packages"
    mkdir -p "$tmp/packages/app/src" "$tmp/packages/game_domain/src"
    printf '%s\n' 'pub struct SafeDomainType;' > "$tmp/packages/game_domain/src/lib.rs"
}

reset_fixture
printf '%s\n' 'use actix_web::HttpRequest;' > "$tmp/packages/app/src/lib.rs"
assert_rejected "a direct Actix import"

reset_fixture
printf '%s\n' 'const QUERY: &str = "SELECT * FROM games";' > "$tmp/packages/app/src/lib.rs"
assert_rejected "raw SQL"

reset_fixture
printf '%s\n' 'console.log("owned client path");' > "$tmp/packages/app/client.js"
assert_rejected "application-owned JavaScript"

reset_fixture
printf '%s\n' '[features]' 'actix = ["hyperchad/renderer-html-sse"]' > "$tmp/packages/app/Cargo.toml"
assert_rejected "a parallel renderer SSE path"

reset_fixture
printf '%s\n' 'use hyperchad::router::Router;' > "$tmp/packages/game_domain/src/lib.rs"
assert_rejected "a HyperChad dependency in the game domain"

printf '%s\n' "architecture checker self-tests passed"
