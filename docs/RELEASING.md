# Releasing

How to cut a Herdr Preview release. A `v*` tag push triggers
`.github/workflows/release.yml`. Managed `herdr plugin install` builds the tagged checkout from
source through `herdr/build.sh`; it never downloads an upstream release binary.

## The one rule

**The manifest version and the tag must match.** A `0.2.0` manifest needs a `v0.2.0` tag so the
installed plugin identity and source release agree.

Two files carry the version — keep them equal:

- `Cargo.toml` → `[package] version`
- `herdr-plugin.toml` → `version`

## Steps

Pick the new version with semver: a behavior change or new feature is a minor bump in `0.x`
(`0.1.1 → 0.2.0`); a fix-only release is a patch (`0.2.0 → 0.2.1`).

1. **Bump both versions** to the new `X.Y.Z` — `Cargo.toml` and `herdr-plugin.toml`.
2. **Finalize the changelog** — rename `## [Unreleased]` to `## [X.Y.Z] — <date>` and add a fresh
   empty `## [Unreleased]` above it. The format is [Keep a Changelog](https://keepachangelog.com).
3. **Refresh the lock** — `cargo build` so `Cargo.lock`'s `herdr-reviewr` entry updates to `X.Y.Z`.
4. **Verify green** — `just ci` (fmt-check, clippy, test, release build).
5. **Commit** the bump + changelog on a branch, review, and land it on `main`.
6. **Tag and push** — an annotated tag whose name is `vX.Y.Z`:

   ```bash
   git checkout main && git pull
   git tag -a vX.Y.Z -m "Herdr Preview vX.Y.Z"
   git push origin main
   git push origin vX.Y.Z        # triggers release.yml
   ```

7. **Watch the build** and confirm the assets landed:

   ```bash
   gh run watch                  # the release.yml run for the tag
   gh release view vX.Y.Z        # four <target>.tar.gz + .sha256 sidecars
   ```

## What the tag triggers

`release.yml` (on `push: tags: ["v*"]`):

- creates the Release **as a draft** with the tag's `CHANGELOG.md` section as its body
  (`taiki-e/create-gh-release-action`). A tag with no matching changelog section fails the
  release — finalize the changelog before tagging;
- builds `herdr-preview` for `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu`;
- uploads each as `herdr-preview-<target>.tar.gz` with a `.sha256` sidecar;
- publishes the draft only after every target's assets attached. The repo's releases are
  immutable — assets cannot be added after publish — so publish is the last step, and a
  failed target leaves an editable draft instead of a sealed, assetless release.

The toolchain is pinned by `rust-toolchain.toml`, so CI and local builds match.

## Gotcha: a tag name is single-use

Releases are immutable. Deleting a failed release does not free its tag name — the repository
rules reserve it forever. A release that must be recut ships under the next patch version.

## Reinstall locally after a release

Switch your own machine from the dev link to the published release. This is also the cheapest
end-to-end test: it exercises the exact `herdr plugin install` path a user hits.

1. **Swap the link for the release.** Your config survives — `config.toml` lives in
   `~/.config/herdr/plugins/config/pi-dal.herdr-preview/`, keyed by plugin id, untouched by a reinstall.

   ```bash
   herdr plugin unlink pi-dal.herdr-preview
   herdr plugin install pi-dal/herdr-preview --yes
   herdr plugin list --plugin pi-dal.herdr-preview   # confirm: github source + version X.Y.Z
   ```

2. **Relaunch the Preview pane** so it runs the new binary instead of the old process.
   A running pane keeps its old binary image until it closes. Closing is safe to script:

   ```bash
   herdr plugin action invoke close --plugin pi-dal.herdr-preview   # closes the focused workspace's Preview panes
   ```

   **Reopen with your own toggle keybinding, never a scripted `open`.** The `open` and `toggle`
   actions act on the focused workspace and ignore `HERDR_WORKSPACE_ID`, so a scripted reopen
   stacks panes into whatever space you happen to be looking at rather than the ones you
   closed (`../AGENTS.md`).

## Notes

- **`min_herdr_version`** (in `herdr-plugin.toml`) only changes when a release depends on a newer
  herdr API. A normal feature release leaves it as is.
- **Code signing** is handled by the shared fresh-inode swap helper used by `herdr/build.sh` and
  `just install`. This avoids the Apple-Silicon SIGKILL caused by overwriting an executable inode.
- **QA against the installed plugin** uses `just qa-install`, never a bare `cp`. Overwriting the
  installed binary in place invalidates its cached code signature, macOS SIGKILLs every launch,
  and the pane opens dead with no error — the recipe replaces the inode and ad-hoc re-signs.
  `just qa-restore` puts the released binary back.
- **`--verify-tag`** means the tag must exist on the remote before the Release is created — push
  the tag, don't create the Release by hand first.
