# AGENTS.md

This repo builds **Shiki**, whose terminal colours drift slowly with local sunrise, sunset and the seasons, like a window onto the day outside. The CLI binary and crate are **`shiki-term`**. The Homebrew formula is `shiki-term`, the menu bar cask is `shiki-term-app` and the bundle id is `io.github.quiinthieu.shiki-term`. Names are set in `crates/sk-core/src/product.rs` and `macos/Config/Product.xcconfig`. It's a Rust workspace (`sk-core` solar/colour/palette math, `sk-store`, `sk-deliver` terminal backends, `sk-import` theme parsers, and the `shiki-term` CLI/daemon) plus an optional thin Swift 6 SwiftUI menu bar app in `macos/`. It isn't related to the shikijs syntax highlighter.

**The source of truth is [`PLAN.md`](./PLAN.md). Read §0 "Instructions for agents" before doing anything.**

Short version:
- Repo: `git@github.com:quiinthieu/shiki.git`. Branch from `main` as `task/TNN-slug`, one task per PR, Conventional Commits, squash-merge after CI passes (PLAN "Repository & workflow").
- Work on the lowest-numbered unchecked task in `PLAN.md` §10, and only that task. When its acceptance criteria pass, tick it and update `CHANGELOG.md`.
- Write tests first. Use `TestEnv` (temp `HOME` and `SHIKI_HOME`) and fixed instants. Never touch the real `$HOME`, rc files or terminal configs.
- `sk-core` is pure: no I/O, no clock, no env. Only `sk-store` writes Shiki's files. rc and terminal configs are edited only with `--write-rc`/`--write-config`, inside a marked block that can be removed.
- Privacy: no network, ever. Location is manual or from the timezone table, and coordinates never appear in logs or `snapshot.json`. `scripts/lint-privacy.sh` enforces the no-network rule.
- Contrast: every frame must pass the WCAG checks in PLAN §4 (fg/bg ≥ 4.5:1, ANSI ≥ 3:1 by default). Property tests sweep a whole year.
- Respect the power budget (PLAN §7). Never invent stats, performance numbers or terminal behaviour. Anything marked *(verify)* gets checked and recorded in `docs/terminals/`.
- Commands: `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`, `cargo deny check`, `scripts/lint-privacy.sh`, and for the macOS app `xcodebuild -project macos/Shiki.xcodeproj -scheme Shiki test`.
