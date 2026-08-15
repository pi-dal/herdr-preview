# herdr-review dev tasks — run `just <task>` (https://github.com/casey/just)
# Every Rust command is explicit about the repository-required toolchain: host `cargo` may be old.
set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# default: list tasks
default:
    @just --list

# format the code
fmt:
    mise exec rust@1.97.1 -- cargo fmt --all

# check formatting (CI parity)
fmt-check:
    mise exec rust@1.97.1 -- cargo fmt --all --check

# lint with clippy, warnings as errors
lint:
    mise exec rust@1.97.1 -- cargo clippy --all-targets --all-features -- -D warnings

# run the test suite
test:
    mise exec rust@1.97.1 -- cargo test --all-features

# build (debug)
build:
    mise exec rust@1.97.1 -- cargo build --locked

# run reviewr in the current repo
run:
    mise exec rust@1.97.1 -- cargo run

# build release and install the binary into bin/ for `herdr plugin link`
install:
    mise exec rust@1.97.1 -- cargo build --release --locked
    mkdir -p bin
    ./scripts/swap-binary.sh target/release/herdr-preview bin/herdr-preview

# build release and swap it into the GitHub-installed plugin for local QA (docs/qa-install.md)
qa-install:
    mise exec rust@1.97.1 -- cargo build --release --locked
    ./scripts/qa-install.sh

# restore the released binary the last `just qa-install` replaced
qa-restore:
    #!/usr/bin/env sh
    set -eu
    bin="$(ls -d "$HOME"/.config/herdr/plugins/github/pi-dal.herdr-preview-*/bin/herdr-preview | head -1)"
    ./scripts/swap-binary.sh "$bin.release-backup" "$bin"
    echo "restored release binary at $bin"

# everything CI runs, locally
ci: fmt-check lint test
    mise exec rust@1.97.1 -- cargo build --release --locked
