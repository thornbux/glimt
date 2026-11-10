#!/bin/sh
# Release tests: builds this checkout (or NEW) and the last release (or OLD) with every feature
# in Docker, and runs tests.sh against them under a virtual X display. Prints PASS or FAIL for
# each check and the timings of both; screenshots go to the work folder's out/.
#
#   tools/release-tests/run.sh [NEW] [OLD]      NEW: a git ref, default HEAD
#                                               OLD: a git ref, default the tag before NEW
#   QUICK=1 tools/release-tests/run.sh          leaves out the large folder and EXIF timings
#   PARTS=N tools/release-tests/run.sh          the checks in N containers at once (default: by
#                                               Docker's processors and memory, 1 to 6)
#
# One container installs the tools and builds both, and is kept as an image; the checks then
# run in PARTS containers of it at once, each with its own X display, /tmp and HOME, in runs of
# whole sections in their order; a section makes what it needs itself. The timings
# run after them with nothing else running, then cargo audit. The logs are printed in order.
#
# The work folder is under $HOME, as Docker on macOS (colima) only shares the home folder.
# The volumes glimt-target and glimt-cargo keep the builds and crates between runs, and the
# build of each OLD by its commit, as a release never changes. The real
# files of samples.txt are downloaded once to ~/.cache/glimt-samples.
set -e
cd "$(git rev-parse --show-toplevel)"
# An interrupted run left its container and its image of 2 to 3 GB, which docker image prune
# never removes, as the image has a name. They go however the run ends.
image=glimt-release-tests:$$
leave() {
    # The trap runs under set -e, and GNU xargs runs docker rm once without names when no
    # container is left, which fails, so without this the image would never be removed.
    set +e
    # By the whole name, so another run's containers, of a pid that starts with this one's, stay.
    docker ps -a --format '{{.Names}}' | grep -E "^glimt-release-tests-$$(-[a-z0-9]+)?\$" |
        xargs docker rm -f >/dev/null 2>&1
    docker rmi -f $image >/dev/null 2>&1
}
trap leave EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# The build folders of removed worktrees fill Docker's disk, and a run with too little room
# fails halfway through its builds, so they go first and the room left is checked.
tools/check.sh --clean || true
free=$(docker run --rm -v glimt-target:/target rust:1 df -Pk /target |
    awk 'NR == 2 { print int($4 / 1048576) }') || free=
if [ -n "$free" ] && [ "$free" -lt 25 ]; then
    echo "Docker has $free GB free, and a run needs about 25 GB: see docker system df"
    exit 1
fi
new=${1:-HEAD}
old=${2:-$(git describe --tags --abbrev=0 "$new^")}
work=${XDG_CACHE_HOME:-$HOME/.cache}/glimt-release-tests
rm -rf "$work"
mkdir -p "$work/new" "$work/old" "$work/out"
git archive "$new" | tar -xm -C "$work/new"
git archive "$old" | tar -xm -C "$work/old"
# The tests of NEW as committed, not the working tree's, so a run checks that commit only.
cp "$work/new/tools/release-tests/tests.sh" "$work/new/tools/release-tests/samples.txt" "$work/"
samples=${XDG_CACHE_HOME:-$HOME/.cache}/glimt-samples
mkdir -p "$samples"
sum() { { sha256sum "$1" 2>/dev/null || shasum -a 256 "$1"; } | cut -d' ' -f1; }
grep -v '^#' "$work/samples.txt" | while read -r name sha url; do
    f=$samples/$name
    [ -f "$f" ] && [ "$(sum "$f")" = "$sha" ] && continue
    if curl -fsSL -o "$f.part" "$url" && [ "$(sum "$f.part")" = "$sha" ]; then
        mv "$f.part" "$f"
    else
        rm -f "$f.part" "$f"
        echo "could not download $name (or its sha256 differs)"
    fi
done
echo "new: $(git rev-parse --short "$new") ($new), old: $old, work: $work"
# Each part keeps about two processors busy (Glimt drawing without a GPU, X, the script) and
# needs about 2.5 GB: as many as Docker has room for, so a small laptop runs them one at a time.
cpus=$(docker info --format '{{.NCPU}}')
gb=$(($(docker info --format '{{.MemTotal}}') / 1073741824))
fit=$((cpus / 2 < gb * 2 / 5 ? cpus / 2 : gb * 2 / 5))
parts=${PARTS:-$((fit < 1 ? 1 : fit > 6 ? 6 : fit))}
echo "parts: $parts (Docker has $cpus processors, $gb GB)"
# The sections of tests.sh: the part before the first, the sections, the end.
python3 - "$work" "$parts" <<'PY'
import re, sys
work, parts = sys.argv[1], int(sys.argv[2])
s = open(f"{work}/tests.sh").read()
user = s.index('\n# ----------------------------------------------------------------')
# A NEW from before 0.58.0 marks its sections with echo "-- …", and each part starts as NEW.
old = '\nsection "' not in s[user:]
mark, start = ('echo "-- ', "\nas_version new\n") if old else ('section "', "")
first = s.index("\n" + mark, user)
end = s.index('\necho "-- not tested here')
head, body = s[:first], s[first:end]
sections = [x for x in re.split("\n(?=" + re.escape(mark) + ")", body) if x.strip()]
timings = [x for x in sections if x.startswith(mark + "timings")]
checks = [x for x in sections if not x.startswith(mark + "timings")]
# Runs of sections of about the same work: a start of Glimt is most of a check's time.
weight = [1 + x.count("start ") * (2 if "for v in new old" in x else 1) for x in checks]
total, runs, run, done = sum(weight), [], [], 0
for x, w in zip(checks, weight):
    run.append(x)
    done += w
    if done >= total * (len(runs) + 1) / parts and len(runs) < parts - 1:
        runs.append(run)
        run = []
runs.append(run)
summary = '\nif [ $FAILED -gt 0 ]; then echo "$FAILED checks failed"; exit 1; fi\n'
for k, run in enumerate(runs + [timings]):
    open(f"{work}/part-{k}.sh", "w").write(head + start + "\n".join(run) + summary)
PY
status=0
docker run --name glimt-release-tests-$$ -e STEP=prepare -e OLD_COMMIT="$(git rev-parse "$old^{commit}")" \
    -v "$work:/t" -v glimt-target:/work-target -v glimt-cargo:/usr/local/cargo/registry \
    rust:1 bash /t/tests.sh || status=$?
docker commit glimt-release-tests-$$ $image >/dev/null || status=1
docker rm glimt-release-tests-$$ >/dev/null
[ $status = 0 ] || { echo "FAIL preparing the tests"; exit 1; }
set +e
# part: K. --privileged gives the container a loop device, for the FAT file system of a camera
# card that tests.sh mounts, /dev/fuse, for the rclone and sftp:// folders and the share that
# loses its replies, and the right to mount the small tmpfs that the checks fill (faults=yes).
# Only the parts with those checks get it, as privileged containers started at the same moment
# race for the loop devices.
part() {
    privileged=$(grep -q '^card=/mnt/card\|^fuse=yes\|^faults=yes' "$work/part-$1.sh" && echo --privileged)
    docker run --rm --name glimt-release-tests-$$-$1 $privileged --shm-size=1g \
        -e STEP=run -e SCRIPT=/t/part-$1.sh -e QUICK="${QUICK:-}" \
        -v "$work:/t" -v "$samples:/samples:ro" $image bash /t/tests.sh > "$work/part-$1.log" 2>&1
}
pids=""
for k in $(seq 0 $((parts - 1))); do part $k & pids="$pids $!"; done
failed=0
for p in $pids; do wait $p || failed=1; done
part $parts || failed=1
for k in $(seq 0 $parts); do grep -v '^ *$' "$work/part-$k.log" | grep -v 'checks failed$'; done
docker run --rm --name glimt-release-tests-$$-audit -e STEP=audit \
    -v "$work:/t" -v glimt-cargo:/usr/local/cargo/registry $image \
    bash /t/tests.sh > "$work/audit.log" 2>&1 || failed=1
cat "$work/audit.log"
echo "-- not tested here, by hand: the Flatpak (two windows with Ctrl+N, --remote next to the"
echo "   focused one, --remote-all next to both, --remote-all quit)"
fails=$(cat "$work"/part-*.log "$work/audit.log" | grep -c '^FAIL')
if [ $failed = 1 ]; then echo "$fails checks failed"; exit 1; fi
echo "every check passed"
