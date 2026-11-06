#!/bin/bash
# The checks before a commit (AGENTS.md) in Docker, for a computer without cargo, or for several
# worktrees at once:
#
#   tools/check.sh                 cargo fmt, then clippy and the tests, without and with every
#                                  feature, in --profile quick; exits nonzero when one fails
#   tools/check.sh 'COMMANDS'      other cargo commands in the same container, such as
#                                  'cargo test --profile quick NAME'
#
# The image (rust:1 with mold, which links in a fraction of the time, and sccache, which keeps
# the crates' builds for every worktree) is built as glimt-check, and again when rust:1 moves on.
# Each checkout has a target folder of its own in the volume glimt-target, so worktrees do not
# wait for each other's lock.
# At most one check per 5 GB of Docker's memory runs at once; the others wait their turn.
# cargo fmt changes the files in place: the checkout is mounted, not copied.
set -u
cd "$(dirname "$0")/.." || exit 2
here=$(pwd)
name=$(basename "$here")-$(printf %s "$here" | cksum | cut -d' ' -f1)
# The image is checked against rust:1 once a day, and built again when rust:1 has moved on, so
# its clippy keeps up with CI's stable. Without the network the image there is used.
stamp=$HOME/.cache/glimt-check-pulled
image=$(docker image inspect glimt-check >/dev/null 2>&1 && echo yes)
if [ -z "$image" ] || [ -z "$(find "$stamp" -mtime -1 2>/dev/null)" ]; then
    docker pull -q rust:1 >/dev/null 2>&1
    base=$(docker image inspect -f '{{.Id}}' rust:1 2>/dev/null)
    built=$(docker image inspect -f '{{index .Config.Labels "glimt.base"}}' glimt-check 2>/dev/null)
    if [ -z "$image" ] || { [ -n "$base" ] && [ "$base" != "$built" ]; }; then
        echo "building glimt-check from rust:1"
        docker build -q --label "glimt.base=$base" -t glimt-check - >/dev/null \
            <<'DOCKERFILE' || exit 2
FROM rust:1
RUN apt-get update && apt-get install -y --no-install-recommends mold sccache \
    && rm -rf /var/lib/apt/lists/* && rustup component add rustfmt clippy
DOCKERFILE
    fi
    mkdir -p "$HOME/.cache" && touch "$stamp"
fi
# A turn among the others: a folder made is a slot taken, and it goes when this ends. It holds
# the taker's process id, so a slot left by a check that was killed is taken over.
mem=$(docker info --format '{{.MemTotal}}' 2>/dev/null)
gb=$((${mem:-0} / 1073741824))
slots=$((gb / 5 < 1 ? 1 : gb / 5))
locks=$HOME/.cache/glimt-check-slots
mkdir -p "$locks"
slot=""
told=""
while [ -z "$slot" ]; do
    for s in $(seq 1 $slots); do
        owner=$(cat "$locks/$s/pid" 2>/dev/null)
        if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
            rm -rf "$locks/$s"
        fi
        if mkdir "$locks/$s" 2>/dev/null; then
            echo $$ >"$locks/$s/pid"
            slot=$s
            break
        fi
    done
    if [ -z "$slot" ]; then
        [ -z "$told" ] && echo "waiting for one of $slots checks running in $locks to end" && told=1
        sleep 10
    fi
done
trap 'rm -rf "$locks/$slot"' EXIT
checks='r=0
step() { echo "-- $*"; "$@" || { echo "FAIL: $*"; r=1; }; }
step cargo fmt
step cargo clippy -q --locked --profile quick --all-targets -- -D warnings
step cargo test -q --locked --profile quick
step cargo clippy -q --locked --profile quick --all-targets --all-features -- -D warnings
step cargo test -q --locked --profile quick --all-features
[ $r = 0 ] && echo "every check passed"
exit $r'
# --init passes Ctrl+C on to cargo, which bash as the container's first process would not.
docker run --rm --init -v "$here":/s -w /s \
    -v glimt-target:/target -v glimt-cargo:/usr/local/cargo/registry -v glimt-sccache:/sccache \
    -e "CARGO_TARGET_DIR=/target/$name" -e RUSTC_WRAPPER=sccache -e SCCACHE_DIR=/sccache \
    -e RUSTFLAGS="-C link-arg=-fuse-ld=mold" -e HOME=/tmp \
    glimt-check bash -c "${1:-$checks}"
