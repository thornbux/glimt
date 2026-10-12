#!/bin/sh
# Release tests: builds this checkout (or NEW) and the last release (or OLD) with every feature
# in Docker, and runs tests.sh against them under a virtual X display. Prints PASS or FAIL for
# each check and the timings of both; screenshots go to the work folder's out/.
#
#   tools/release-tests/run.sh [NEW] [OLD]      NEW: a git ref, default HEAD
#                                               OLD: a git ref, default the tag before NEW
#   QUICK=1 tools/release-tests/run.sh          leaves out the large folder and EXIF timings
#
# The work folder is under $HOME, as Docker on macOS (colima) only shares the home folder.
# The volumes glimt-target and glimt-cargo keep the builds and crates between runs.
set -e
cd "$(git rev-parse --show-toplevel)"
new=${1:-HEAD}
old=${2:-$(git describe --tags --abbrev=0 "$new^")}
work=${XDG_CACHE_HOME:-$HOME/.cache}/glimt-release-tests
rm -rf "$work"
mkdir -p "$work/new" "$work/old" "$work/out"
git archive "$new" | tar -x -C "$work/new"
git archive "$old" | tar -x -C "$work/old"
cp tools/release-tests/tests.sh "$work/"
echo "new: $(git rev-parse --short "$new") ($new), old: $old, work: $work"
docker run --rm --shm-size=1g -e QUICK="${QUICK:-}" -v "$work:/t" \
    -v glimt-target:/work-target -v glimt-cargo:/usr/local/cargo/registry \
    rust:1 bash /t/tests.sh
