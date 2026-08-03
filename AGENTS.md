# Words with More Than Friends contributor instructions

## Architectural boundaries

`INVARIANTS.md` is binding. In particular, application code must remain renderer-neutral, gameplay must remain server-authoritative, hidden state must remain private, and persistence must use `switchy` builders rather than raw SQL.

- Put deterministic gameplay rules in `packages/game_domain`; that crate must not depend on HyperChad, persistence, accounts, HTTP, or a renderer.
- Put renderer-neutral routes, pages, and orchestration in `packages/app`.
- Select HTML/Actix only through root features and runtime wiring. Never import `actix-web` in application code.
- Add a crate only when a concrete ownership or dependency boundary requires it. Do not create generic `core`, `common`, or `shared` crates.
- Use `BTreeMap` and `BTreeSet` for deterministic collections.
- Declare third-party dependencies once in the root workspace table with full versions, `default-features = false`, and narrow features. Leaf crates use `workspace = true`.
- Every crate must expose `fail-on-warnings = []` and use the repository lint attributes.
- Do not commit application-owned JavaScript or a live-update path parallel to HyperChad.
- Treat `wwmtf.md` as a local progress document unless the user explicitly chooses to commit it.

## Required validation

Run these after relevant changes:

```sh
cargo fmt --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
cargo machete --with-metadata
./scripts/check-architecture.sh
./scripts/test-architecture-checks.sh
```

Use `cargo test --workspace` only when `cargo-nextest` is unavailable.
