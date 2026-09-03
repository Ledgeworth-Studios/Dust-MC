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

# The long one: a bot that stays, flies a square, digs and talks for a while,
# and says whether anything ended or went quiet. Phase 3's exit criterion asks
# for ten minutes, which is the default.
#
# Outside `verify` for the same reasons `bot` is, plus one of its own: ten
# minutes is not a pull-request gate.
soak port="25565" minutes="10":
    cd tools/bot && node soak.js {{port}} {{minutes}}

# What a real client's movement packets actually contain, as counts — and,
# with `check`, whether the server corrects one that lies.
#
# Outside `verify` for the same reason `bot` is: it needs a server already
# running and an npm install. It is what decision record 0012's table came
# from, and re-running it is how a change to `[server] movement_speed_limit`
# is argued about with numbers rather than opinions.
movement port="25565" check="":
    cd tools/bot && node movement.js {{port}} {{ if check == "check" { "--check" } else { "" } }}

# Whether a player who claims to be standing inside a block is put back —
# and, as the control that makes the answer mean anything, whether a move of
# the same length that ends in open air is left alone.
#
# Outside `verify` for the same reason `bot` is. Run it against both worlds:
# a flat one and a `world_source` of region files exercise different halves
# of the world lookup, and the second is the one with a column cache in it.
collide port="25565":
    cd tools/bot && node collide.js {{port}}
