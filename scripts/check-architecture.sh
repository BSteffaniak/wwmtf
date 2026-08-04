#!/bin/sh
set -eu

fail=0

report_violation() {
    printf '%s\n' "architecture violation: $1" >&2
    fail=1
}

check_pattern() {
    label=$1
    pattern=$2
    shift 2

    if grep -R -n -E "$pattern" "$@" --include='*.rs' --include='*.toml' --exclude-dir=target 2>/dev/null; then
        report_violation "$label"
    fi
}

javascript_files=$(find packages -type f \( -name '*.js' -o -name '*.mjs' -o -name '*.cjs' -o -name '*.ts' \) -print)
if [ -n "$javascript_files" ]; then
    printf '%s\n' "$javascript_files"
    report_violation "application-owned JavaScript or TypeScript"
fi

check_pattern "direct Actix dependency or import" '(^|[^[:alnum:]_])(actix-web|actix_web)([^[:alnum:]_]|$)' packages
check_pattern "raw SQL in application code" '(SELECT|INSERT[[:space:]]+INTO|UPDATE[[:space:]]+[[:alnum:]_]+[[:space:]]+SET|DELETE[[:space:]]+FROM|CREATE[[:space:]]+TABLE|ALTER[[:space:]]+TABLE|DROP[[:space:]]+TABLE)' packages
check_pattern "parallel renderer SSE path" 'renderer-html-sse|renderer-vanilla-js-plugin-sse' packages/app/Cargo.toml
check_pattern "renderer-specific application branch" 'cfg!?.*renderer|feature[[:space:]]*=[[:space:]]*"(actix|egui|fltk|lambda)"' packages/app/src
check_pattern "game-domain dependency points out of the domain" '(^|[^[:alnum:]_])(hyperchad|switchy|actix[-_]|database|http)([^[:alnum:]_]|$)' packages/game_domain
check_pattern "forbidden generic crate name" '^name[[:space:]]*=[[:space:]]*"(core|common|shared)"' packages

if [ "$fail" -ne 0 ]; then
    exit 1
fi

printf '%s\n' "architecture checks passed"
