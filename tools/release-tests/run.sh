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
# One container installs the tools, kept as an image of their own until they change; one of it
# builds both, and is kept as an image; the checks then
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
# A run with too little room fails halfway through its builds, so the room is checked first.
# The build folders of removed worktrees fill Docker's disk, but run.sh does not remove them:
# tools/check.sh --clean also removes those of other clones, which AGENTS.md says to ask about.
free=$(docker run --rm -v glimt-target:/target rust:1 df -Pk /target |
    awk 'NR == 2 { print int($4 / 1048576) }') || free=
if [ -n "$free" ] && [ "$free" -lt 25 ]; then
    echo "Docker has $free GB free, and a run needs about 25 GB: see docker system df, and"
    echo "tools/check.sh --clean, which also removes the build folders of other clones"
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
# The seconds each section took in an earlier run, by which the sections are shared out.
cp "$work/new/tools/release-tests/section-times.txt" "$work/" 2>/dev/null || :
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
# Each section's seconds in an earlier run (section-times.txt, "seconds<tab>title", from the
# "took" notes), so that no part holds the run back. A section not in it, new or renamed, is
# weighed by its starts of Glimt, which are most of a check's time, at the seconds a start took
# on average in the sections that are in it.
try:
    lines = open(f"{work}/section-times.txt").read().splitlines()
    seconds = {t: int(n) for n, t in (line.split("\t", 1) for line in lines if line.strip())}
except OSError:
    seconds = {}
def title(x):
    return x.split("\n", 1)[0][len(mark):].rstrip('"')
# A part may start at any section, so a function that one section defines and another calls
# is command not found there, unless it is defined in the head. The shell's own text is
# searched: comments, heredocs, python3 -c scripts and $(( )) are left out.
def code(x):
    x = re.sub(r"<<-?'?(\w+)'?[^\n]*\n.*?\n\s*\1\n", "\n", x, flags=re.S)
    x = re.sub(r'python3 -c "[^"]*"', "", x)
    x = re.sub(r"\$\(\(.*?\)\)", "", x)
    return re.sub(r"(?m)^\s*#.*$", "", x)
define = re.compile(r"(?m)^\s*(\w+)\s*\(\)\s*\{")
own = [set(define.findall(code(x))) for x in checks]
in_sections = set().union(*own) - set(define.findall(head))
calls = [f"{title(x)}: {f}" for x, mine in zip(checks, own) for f in sorted(in_sections - mine)
         if re.search(r"(?m)(^|[;&|({!]|\$\(|\bthen|\bdo|\belse)\s*" + f + r"(?=[\s;]|$)(?!\s*=)"
                      r"|\$\(\s*" + f + r"\s*\)", code(x))]
if calls:
    sys.exit("FAIL a section calls a function that another section defines; define it in the"
             " head of tests.sh:\n" + "\n".join(calls))
starts = [1 + x.count("start ")* (2 if "for v in new old" in x else 1) for x in checks]
known = [(seconds[title(x)], n) for x, n in zip(checks, starts) if title(x) in seconds]
per_start = sum(s for s, _ in known) / sum(n for _, n in known) if known else 1
weight = [seconds.get(title(x), n * per_start) for x, n in zip(checks, starts)]
# The longest first, each into the part with the least so far; a part keeps the file's order.
load, chosen = [0] * parts, [[] for _ in range(parts)]
for i in sorted(range(len(checks)), key=lambda i: -weight[i]):
    k = load.index(min(load))
    load[k] += weight[i]
    chosen[k].append(i)
runs = [[checks[i] for i in sorted(c)] for c in chosen]
summary = '\ntook\nif [ $FAILED -gt 0 ]; then echo "$FAILED checks failed"; exit 1; fi\n'
for k, run in enumerate(runs + [timings]):
    open(f"{work}/part-{k}.sh", "w").write(head + start + "\n".join(run) + summary)
PY
# The tools are installed once into an image of their own, kept by a key of the block of
# tests.sh that installs them and of rust:1, so a change to either makes it again.
tools_key=$( { sed -n '/-- installing tools/,/STEP" = tools/p' "$work/tests.sh"
    docker image inspect -f '{{.Id}}' rust:1; } | { sha256sum 2>/dev/null || shasum -a 256; } |
    cut -c1-16)
tools=glimt-release-tools:$tools_key
if ! docker image inspect $tools >/dev/null 2>&1; then
    docker run --name glimt-release-tests-$$ -e STEP=tools -v "$work:/t" rust:1 bash /t/tests.sh &&
        docker commit glimt-release-tests-$$ $tools >/dev/null ||
        { docker rm -f glimt-release-tests-$$ >/dev/null; echo "FAIL installing the tools"; exit 1; }
    docker rm glimt-release-tests-$$ >/dev/null
    # Only the latest is kept, as an older key is not asked for again.
    docker images --format '{{.Repository}}:{{.Tag}}' glimt-release-tools | grep -vx "$tools" |
        xargs docker rmi >/dev/null 2>&1 || :
fi
status=0
docker run --name glimt-release-tests-$$ -e STEP=prepare -e OLD_COMMIT="$(git rev-parse "$old^{commit}")" \
    -v "$work:/t" -v glimt-target:/work-target -v glimt-cargo:/usr/local/cargo/registry \
    $tools bash /t/tests.sh || status=$?
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
