#!/bin/bash
# The checks before a commit (AGENTS.md) in Docker, for a computer without cargo, or for several
# worktrees at once:
#
#   tools/check.sh                 cargo fmt, then clippy and the tests, without and with every
#                                  feature, in --profile quick; exits nonzero when one fails
#   tools/check.sh 'COMMANDS'      other cargo commands in the same container, such as
#                                  'cargo test --profile quick NAME'
#   tools/check.sh --clean         removes the build folders of checkouts that are gone
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
folder() { printf '%s-%s' "$(basename "$1")" "$(printf %s "$1" | cksum | cut -d' ' -f1)"; }
name=$(folder "$here")
# Each checkout's build folder stays in glimt-target when its worktree is removed, 1.5 to 4.5 GB
# each, until the disk is full. --clean keeps those of the worktrees git still knows and
# run.sh's own, and removes the rest.
if [ "${1:-}" = --clean ]; then
    keep="release-tests-new release-tests-old"
    while IFS= read -r tree; do
        keep="$keep $(folder "$tree")"
    done < <(git worktree list --porcelain | sed -n 's/^worktree //p')
    exec docker run --rm -v glimt-target:/target -w /target glimt-check sh -c '
        for d in *; do
            [ -d "$d" ] || continue
            case " $1 " in *" $d "*) continue ;; esac
            echo "removing $d ($(du -sh "$d" | cut -f1))"
            rm -rf "$d"
        done
        df -h /target | tail -1' sh "$keep"
fi
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
        # The rust:1 and glimt-check that were replaced are left without a name; about 4 GB.
        docker image prune -f >/dev/null
    fi
    mkdir -p "$HOME/.cache" && touch "$stamp"
fi
# A turn among the others: each slot is a file locked by the docker command that runs the
# checks. The kernel lets go of the lock when that process ends, however it ends, so no slot is
# left taken by a check that was killed, and two waiting checks cannot both take the same slot.
mem=$(docker info --format '{{.MemTotal}}' 2>/dev/null)
gb=$((${mem:-0} / 1073741824))
slots=$((gb / 5 < 1 ? 1 : gb / 5))
locks=$HOME/.cache/glimt-check-locks
mkdir -p "$locks"
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
# Perl takes the lock, because macOS has no flock command; $^F keeps its file open across exec.
perl -e 'use Fcntl ":flock"; $^F = 255; $| = 1;
    my ($dir, $slots) = splice @ARGV, 0, 2; my $told;
    while (1) {
        for my $s (1 .. $slots) {
            open(my $f, ">", "$dir/$s") or die "$dir/$s: $!\n";
            if (flock($f, LOCK_EX | LOCK_NB)) { exec @ARGV; die "$ARGV[0]: $!\n" }
            close $f;
        }
        print "waiting for one of $slots checks running in $dir to end\n" unless $told++;
        sleep 10;
    }' "$locks" "$slots" \
docker run --rm --init -v "$here":/s -w /s \
    -v glimt-target:/target -v glimt-cargo:/usr/local/cargo/registry -v glimt-sccache:/sccache \
    -e "CARGO_TARGET_DIR=/target/$name" -e RUSTC_WRAPPER=sccache -e SCCACHE_DIR=/sccache \
    -e RUSTFLAGS="-C link-arg=-fuse-ld=mold" -e HOME=/tmp \
    glimt-check bash -c "${1:-$checks}"
