#!/bin/bash
# The checks before a commit (AGENTS.md) in Docker, for a computer without cargo, or for several
# worktrees at once:
#
#   tools/check.sh                 cargo fmt --check, then clippy and the tests, without and
#                                  with every feature, in --profile quick, and a cargo check
#                                  in both builds with the oldest Rust that Cargo.toml's
#                                  rust-version names, as CI does; exits nonzero when one fails
#   tools/check.sh 'COMMANDS'      other cargo commands in the same container, such as
#                                  'cargo test --profile quick NAME'
#   tools/check.sh --clean         removes the build folders of checkouts that are gone
#
# The image (rust:1 with mold, which links in a fraction of the time, and sccache, which keeps
# the crates' builds for every worktree) is built as glimt-check, and again when rust:1 moves on.
# Each checkout has a target folder of its own in the volume glimt-target, so worktrees do not
# wait for each other's lock.
# At most one check per 5 GB of Docker's memory runs at once; the others wait their turn.
# Files that cargo fmt --check finds are formatted in place, as the checkout is mounted, not
# copied, and the check fails: run it again on the formatted files.
set -u
cd "$(dirname "$0")/.." || exit 2
# The real path, as git worktree list prints it, so --clean knows this checkout's folder also
# when it is reached through a link (on macOS /tmp is /private/tmp).
here=$(pwd -P)
folder() { printf '%s-%s' "$(basename "$1")" "$(printf %s "$1" | cksum | cut -d' ' -f1)"; }
name=$(folder "$here")
# Each checkout's build folder stays in glimt-target when its worktree is removed, 1.5 to 4.5 GB
# each, until the disk is full. --clean keeps those of the worktrees git still knows and
# run.sh's own (the two builds and the last release's binaries), and removes the rest. A clone
# of the repository elsewhere is not in this list, so its folder goes and is built again.
# A checkout's first check does the same (below), so new worktrees do not fill it between runs.
kept() {
    local trees tree keep="release-tests-new release-tests-old old-builds"
    trees=$(git worktree list --porcelain) || return 1
    while IFS= read -r tree; do
        keep="$keep $(folder "$tree")"
    done < <(printf '%s\n' "$trees" | sed -n 's/^worktree //p')
    printf '%s' "$keep"
}
# The script that removes them, run in /target with the folders to keep as $1.
sweep='for d in *; do
        [ -d "$d" ] || continue
        case " $1 " in *" $d "*) continue ;; esac
        echo "removing $d ($(du -sh "$d" | cut -f1))"
        rm -rf "$d"
    done
    df -h /target | tail -1'
if [ "${1:-}" = --clean ]; then
    keep=$(kept) || { echo "git worktree list failed: nothing removed"; exit 2; }
    # rust:1 has the same shell tools, for a disk freed by removing glimt-check too.
    base=glimt-check
    docker image inspect glimt-check >/dev/null 2>&1 || base=rust:1
    exec docker run --rm -v glimt-target:/target -w /target "$base" sh -c "$sweep" sh "$keep"
fi
# The image is checked against rust:1 once a day, and built again when rust:1 has moved on, so
# its clippy keeps up with CI's stable. Without the network the image there is used. It also
# holds the Rust of Cargo.toml's rust-version, which CI builds with too, and is built again when
# that version changes.
msrv=$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
stamp=$HOME/.cache/glimt-check-pulled
image=$(docker image inspect glimt-check >/dev/null 2>&1 && echo yes)
has_msrv=$(docker image inspect -f '{{index .Config.Labels "glimt.msrv"}}' glimt-check 2>/dev/null)
if [ -z "$image" ] || [ "$has_msrv" != "$msrv" ] ||
    [ -z "$(find "$stamp" -mtime -1 2>/dev/null)" ]; then
    docker pull -q rust:1 >/dev/null 2>&1
    base=$(docker image inspect -f '{{.Id}}' rust:1 2>/dev/null)
    built=$(docker image inspect -f '{{index .Config.Labels "glimt.base"}}' glimt-check 2>/dev/null)
    if [ -z "$image" ] || [ "$has_msrv" != "$msrv" ] ||
        { [ -n "$base" ] && [ "$base" != "$built" ]; }; then
        echo "building glimt-check from rust:1, with Rust $msrv"
        docker build -q --label "glimt.base=$base" --label "glimt.msrv=$msrv" \
            --build-arg "MSRV=$msrv" -t glimt-check - >/dev/null <<'DOCKERFILE' || exit 2
FROM rust:1
ARG MSRV
RUN apt-get update && apt-get install -y --no-install-recommends mold sccache \
    && rm -rf /var/lib/apt/lists/* && rustup component add rustfmt clippy \
    && rustup toolchain install "$MSRV" --profile minimal
DOCKERFILE
        # The rust:1 and glimt-check that were replaced are left without a name; about 4 GB.
        docker image prune -f >/dev/null
    fi
    mkdir -p "$HOME/.cache" && touch "$stamp"
fi
# This checkout's first check makes a new build folder, so the folders of the worktrees removed
# since the last one go first. It is done before waiting for a turn, so the worktrees it keeps
# are those git knows at the moment it removes the others.
if keep=$(kept); then
    docker run --rm -v glimt-target:/target -w /target glimt-check \
        sh -c '[ -d "$2" ] || { '"$sweep"'; }' sh "$keep" "$name"
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
# Unformatted files fail the check, as cargo fmt --check fails them in CI, and are formatted
# for the next run.
formatted() { cargo fmt --check >/dev/null || { cargo fmt; echo "cargo fmt changed files"; false; }; }
step formatted
step cargo clippy -q --locked --profile quick --all-targets -- -D warnings
step cargo test -q --locked --profile quick
step cargo clippy -q --locked --profile quick --all-targets --all-features -- -D warnings
step cargo test -q --locked --profile quick --all-features
step cargo "+$MSRV" check -q --locked
step cargo "+$MSRV" check -q --locked --all-features
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
    -e RUSTFLAGS="-C link-arg=-fuse-ld=mold" -e HOME=/tmp -e "MSRV=$msrv" \
    glimt-check bash -c "${1:-$checks}"
