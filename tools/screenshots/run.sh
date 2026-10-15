#!/bin/sh
# Screenshots for the README and the website: builds this checkout (or NEW) in Docker, as the
# release tests do, and runs shots.sh under the same virtual X display, with generated pictures
# only. The pictures land in docs/screenshots/; compare them with git diff before committing.
#
#   tools/screenshots/run.sh [NEW]      NEW: a git ref, default HEAD
set -e
cd "$(git rev-parse --show-toplevel)"
new=${1:-HEAD}
work=${XDG_CACHE_HOME:-$HOME/.cache}/glimt-screenshots
rm -rf "$work"
mkdir -p "$work/new" "$work/out"
git archive "$new" | tar -x -C "$work/new"
cp tools/release-tests/tests.sh tools/screenshots/shots.sh "$work/"
docker run --rm --shm-size=1g -e SHOTS=1 -v "$work:/t" \
    -v glimt-target:/work-target -v glimt-cargo:/usr/local/cargo/registry \
    rust:1 bash /t/tests.sh
cp $(ls "$work"/out/*.png | grep -v "/x-") docs/screenshots/
echo "screenshots in docs/screenshots: $(ls "$work"/out/*.png | wc -l)"
