# Dust — developer commands.
#
# `just verify` is CI's command list, in CI's order. Not a subset of it: a local
# gate that skips steps produces confidence in exact proportion to what it
# skipped, and the steps it skips are where the failures are.
#
# The CI workflow's step list is derived from these recipes for the same reason.

default:
    @just --list

# Everything CI runs, in CI's order. Run this before every push.
verify: fmt-check lint test docs-check licenses build

# Formatting, as a gate.
fmt-check:
    cargo fmt --all -- --check

# Formatting, applied.
fmt:
    cargo fmt --all

# Lints. Warnings are errors here; a warning nobody has to fix is a warning
# everybody stops reading.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# The test suite.
test:
    cargo test --workspace --all-features

# The generated configuration reference must match the configuration types.
docs-check:
    cargo xtask docs --check

# Regenerate the configuration reference.
docs:
    cargo xtask docs

# Every dependency's licence must be one a GPL-3.0 work may incorporate.
licenses:
    cargo xtask licenses

build:
    cargo build --workspace --all-features

# Point a third-party client at a running server.
#
# NOT part of `verify`, and that is the one recipe here that is deliberately
# outside it: this needs a server already running, an npm install, and a
# `[data] path` in its configuration. `verify` is CI's list in CI's order and
# a step CI cannot run has no business in it.
#
# It is still the first thing to run after any protocol change. mineflayer
# shares no code with this project, which is why it finds what the test suite
# agrees with itself about. See tools/bot/README.md.
bot port="25565":
    cd tools/bot && node check.js {{port}}
