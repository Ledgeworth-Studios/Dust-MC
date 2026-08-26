#!/usr/bin/env bash
#
# Generate the vanilla corpus: region files written by a real Minecraft server.
#
# The tests in `tests/vanilla_corpus.rs` are the only thing in this crate that
# checks Dust's region reader against something Dust did not write. A read
# followed by a write agrees with itself under any self-consistent convention,
# including a wrong one, so the corpus is what makes the rest of the suite
# mean anything. It is Mojang-derived, so it is gitignored and every developer
# regenerates it.
#
# Usage:  crates/dust-world/tools/generate-corpus.sh
#
# This wants to be `cargo xtask corpus` and will be, once the crate that owns
# xtask has landed. It is a shell script today so that it does not touch a
# directory this branch does not own.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"

jar="${DUST_SERVER_JAR:-$root/.dust-extract/server-1.21.1.jar}"
corpus="${DUST_WORLD_CORPUS:-$root/crates/dust-world/.corpus}"
server="$corpus/generate"

# A fixed seed, because a corpus that differs between machines cannot be talked
# about. The string is hashed by the game; the number it becomes is in level.dat.
seed="dust-phase-2"

# How long to let the forceloaded areas generate before saving. There is no
# console message that says "every forced chunk has been generated", so this is
# a wait rather than a wait-for. The tests read whatever the server actually
# wrote, so a short wait yields a smaller corpus, never a wrong one.
generate_seconds="${DUST_CORPUS_SECONDS:-150}"

# The 1.21.1 server jar is a *bundler*: before it runs anything it unpacks its
# libraries and the real server jar into the process's working directory. That
# is why every java invocation below runs with its working directory set inside
# $corpus, which is gitignored, and never at the workspace root.
#
# The guard is a snapshot of the workspace root taken before the run and
# compared after it, rather than a list of the three directory names this jar
# happens to create. A list can only fail on the cases whoever wrote it already
# knew about, and the next jar will drop something else.
root_snapshot() {
  ( cd "$root" && ls -A ) | sort
}
before_root="$(root_snapshot)"

check_root_untouched() {
  local after
  after="$(root_snapshot)"
  local new
  new="$(comm -13 <(printf '%s\n' "$before_root") <(printf '%s\n' "$after"))"
  if [ -n "$new" ]; then
    echo >&2
    echo "the server run left new entries at the workspace root:" >&2
    printf '  %s\n' $new >&2
    echo "delete them. the working directory was wrong, and gitignoring them hides that." >&2
    return 1
  fi
}
trap 'check_root_untouched || exit 1' EXIT

if [ ! -f "$jar" ]; then
  echo "no server jar at $jar" >&2
  echo "set DUST_SERVER_JAR, or run the extractor that populates .dust-extract/" >&2
  exit 1
fi

rm -rf "$server"
mkdir -p "$server"

# Accepting the EULA is the operator's act, not the script's. It is written here
# because this is a throwaway server on the developer's own machine, generating
# test data that is never distributed; a developer who does not accept it should
# not run this script.
echo "eula=true" > "$server/eula.txt"

cat > "$server/server.properties" <<PROPS
level-seed=$seed
level-name=world
online-mode=false
max-players=1
gamemode=creative
difficulty=peaceful
view-distance=10
simulation-distance=10
spawn-protection=0
sync-chunk-writes=true
allow-nether=false
enable-command-block=false
max-tick-time=-1
PROPS

log="$server/console.log"
fifo="$server/console.in"
rm -f "$fifo"
mkfifo "$fifo"

echo "starting the server in $server"
( cd "$server" && exec java -Xmx2G -Xms512M -jar "$jar" nogui ) < "$fifo" > "$log" 2>&1 &
java_pid=$!

# Hold the write end open so the server's console reader does not see EOF the
# moment the first command is delivered.
exec 3> "$fifo"

started=0
for _ in $(seq 1 240); do
  if grep -q 'Done (' "$log" 2>/dev/null; then started=1; break; fi
  if ! kill -0 "$java_pid" 2>/dev/null; then break; fi
  sleep 1
done
if [ "$started" -ne 1 ]; then
  echo "the server never reported Done; tail of $log:" >&2
  tail -40 "$log" >&2
  exit 1
fi
echo "server up: $(grep -m1 'Done (' "$log")"

# Spawn generation alone covers roughly 441 chunks around the origin, which all
# land in one region file. Four forceloaded squares put chunks in four different
# region files, so the header, the sector allocator and the region-coordinate
# arithmetic are all exercised by more than one file. Each square is 16x16
# chunks because vanilla refuses a forceload of more than 256 chunks at once.
for area in "0 0 255 255" "512 0 767 255" "0 512 255 767" "-256 -256 -1 -1"; do
  # shellcheck disable=SC2086
  set -- $area
  echo "forceload add $1 $2 $3 $4" >&3
done

echo "generating for ${generate_seconds}s"
sleep "$generate_seconds"

echo "save-all flush" >&3
for _ in $(seq 1 120); do
  if grep -q 'Saved the game' "$log" 2>/dev/null; then break; fi
  sleep 1
done

echo "stop" >&3
exec 3>&-
wait "$java_pid" || true

regions="$server/world/region"
if [ ! -d "$regions" ]; then
  echo "the server produced no region directory at $regions" >&2
  exit 1
fi

rm -rf "$corpus/world"
mkdir -p "$corpus/world"
cp -R "$regions" "$corpus/world/region"
if [ -f "$server/world/level.dat" ]; then
  cp "$server/world/level.dat" "$corpus/world/level.dat"
fi

echo
echo "corpus written to $corpus/world/region"
ls -la "$corpus/world/region"
