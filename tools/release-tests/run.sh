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
# whole sections in their order (a section may use the files of the one before). The timings
# run after them with nothing else running, then cargo audit. The logs are printed in order.
#
# The work folder is under $HOME, as Docker on macOS (colima) only shares the home folder.
# The volumes glimt-target and glimt-cargo keep the builds and crates between runs, and the
# build of each OLD by its commit, as a release never changes. The real
# files of samples.txt are downloaded once to ~/.cache/glimt-samples.
set -e
cd "$(git rev-parse --show-toplevel)"
new=${1:-HEAD}
old=${2:-$(git describe --tags --abbrev=0 "$new^")}
work=${XDG_CACHE_HOME:-$HOME/.cache}/glimt-release-tests
rm -rf "$work"
mkdir -p "$work/new" "$work/old" "$work/out"
git archive "$new" | tar -xm -C "$work/new"
git archive "$old" | tar -xm -C "$work/old"
cp tools/release-tests/tests.sh tools/release-tests/samples.txt "$work/"
samples=${XDG_CACHE_HOME:-$HOME/.cache}/glimt-samples
mkdir -p "$samples"
sum() { { sha256sum "$1" 2>/dev/null || shasum -a 256 "$1"; } | cut -d' ' -f1; }
grep -v '^#' tools/release-tests/samples.txt | while read -r name sha url; do
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
first = s.index('\necho "-- ', user)
end = s.index('\necho "-- not tested here')
head, body = s[:first], s[first:end]
sections = [x for x in re.split(r'\n(?=echo "-- )', body) if x.strip()]
timings = [x for x in sections if x.startswith('echo "-- timings')]
checks = [x for x in sections if not x.startswith('echo "-- timings')]
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
    open(f"{work}/part-{k}.sh", "w").write(head + "\nas_version new\n" + "\n".join(run) + summary)
PY
image=glimt-release-tests:$$
status=0
docker run --name glimt-release-tests-$$ -e STEP=prepare -e OLD_COMMIT="$(git rev-parse "$old^{commit}")" \
    -v "$work:/t" -v glimt-target:/work-target -v glimt-cargo:/usr/local/cargo/registry \
    rust:1 bash /t/tests.sh || status=$?
docker commit glimt-release-tests-$$ $image >/dev/null || status=1
docker rm glimt-release-tests-$$ >/dev/null
[ $status = 0 ] || { docker rmi $image >/dev/null 2>&1; echo "FAIL preparing the tests"; exit 1; }
set +e
# part: K
part() {
    docker run --rm --shm-size=1g -e STEP=run -e SCRIPT=/t/part-$1.sh -e QUICK="${QUICK:-}" \
        -v "$work:/t" -v "$samples:/samples:ro" $image bash /t/tests.sh > "$work/part-$1.log" 2>&1
}
pids=""
for k in $(seq 0 $((parts - 1))); do part $k & pids="$pids $!"; done
failed=0
for p in $pids; do wait $p || failed=1; done
part $parts || failed=1
for k in $(seq 0 $parts); do grep -v '^ *$' "$work/part-$k.log" | grep -v 'checks failed$'; done
docker run --rm -e STEP=audit -v "$work:/t" -v glimt-cargo:/usr/local/cargo/registry $image \
    bash /t/tests.sh || failed=1
docker rmi $image >/dev/null
echo "-- not tested here, by hand: the Flatpak (two windows with Ctrl+N, --remote next to the"
echo "   focused one, --remote-all next to both, --remote-all quit)"
fails=$(cat "$work"/part-*.log | grep -c '^FAIL')
if [ $failed = 1 ]; then echo "$fails checks failed"; exit 1; fi
echo "every check passed"
