# Contributing

Thanks for wanting to make Herdr Preview better. This page gets you from clone to a reviewed change.

Herdr Preview is a fork of `persiyanov/herdr-reviewr`. The Rust crate retains the
`herdr-reviewr` name to preserve the upstream engine and minimize divergence. The installed
`herdr-preview` executable keeps Preview panes distinct from upstream panes. User-visible plugin,
action, pane, and config identities use `pi-dal.herdr-preview` and Herdr Preview.

## Setup

You need Rust (the exact toolchain is pinned by `rust-toolchain.toml` and installs itself on
first build) and [`just`](https://github.com/casey/just).

```bash
git clone https://github.com/pi-dal/herdr-preview
cd herdr-preview
mise exec rust@1.97.1 -- cargo test --all-features
mise exec rust@1.97.1 -- cargo run -- ~/some/repo
```

`just ci` runs exactly what CI runs: format check, clippy with warnings as errors, tests, and a
release build. Green there means green in CI.

To test a change inside real herdr panes, `just qa-install` swaps your build into the installed
plugin and `just qa-restore` brings the release back. The details and the sharp edges live in
`docs/qa-install.md`.

## How this repo works

**Specs are the contract.** Every behavior lives in a spec under `specs/` (`overview.md` is the
map). A change to behavior lands in the spec and the code together, in the same PR. If you find
the code and a spec disagreeing, that is a bug report worth filing on its own.

**The changelog is written as you go.** User-visible changes add a bullet under `## [Unreleased]`
in `CHANGELOG.md`. That text becomes the release notes verbatim, so write it for the person
reading the release page.

**Performance changes bring numbers.** The PTY benchmark measures what a user feels — keypress to
painted frame:

```bash
python3 scripts/bench_tui.py --binary target/release/herdr-preview --fixture
```

Baselines live in `scripts/bench-results/`. For anything touching the reload, render, git, or
highlight paths, run it before and after under the same system load and put both numbers in the
PR. `examples/bench_latency.rs` attributes a slow number to its component calls.

## Pull requests

- Keep one PR to one concern.
- `just ci` green, spec updated with the behavior, changelog bullet added.
- Tests live beside the code (unit) and in `tests/` (integration, against real git repos).
  Test names read as sentences: `a_tab_switch_paints_the_stashed_frame_and_defers_its_reload`.

## Releasing

Maintainers only — the process is `docs/RELEASING.md` in full. Short version: bump two versions,
finalize the changelog, tag `vX.Y.Z`, and CI builds the binaries and publishes the release with
the changelog section as its notes.
