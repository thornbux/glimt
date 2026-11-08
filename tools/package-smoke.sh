#!/bin/bash
# A smoke test of a package as a user starts it: Glimt shows an SVG, which it draws in a child
# process started from /proc/self/exe (src/images/svg.rs). The release workflow runs it on the
# AppImage and the Flatpak bundle before they are published.
#
#   tools/package-smoke.sh COMMAND...                  COMMAND starts Glimt, such as
#                                                      ./glimt.AppImage or flatpak run APP-ID
#   tools/package-smoke.sh --second-window COMMAND...  also opens a New Window, closes the
#                                                      first, and has the second draw an SVG
#
# The second case is the AppImage's: the binary is in the first window's mount, which goes
# when that window closes. It needs remote control, which is on by default.
# Needs Xvfb, python3 and timeout. The screen is read from Xvfb's own framebuffer file, so no
# screenshot tool is needed. Exits nonzero when a picture is not shown.
set -u
second_window=
if [ "${1:-}" = --second-window ]; then second_window=1; shift; fi
[ $# -gt 0 ] || { echo "usage: $0 [--second-window] COMMAND..." >&2; exit 2; }

# In the working folder, not /tmp, which the Flatpak's sandbox does not share.
dir=$(mktemp -d "$PWD/glimt-smoke.XXXXXX")
pids=()
cleanup() {
    kill "${pids[@]}" 2>/dev/null
    wait 2>/dev/null
    rm -rf "$dir"
}
trap cleanup EXIT
if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
    export XDG_RUNTIME_DIR="$dir/run"
    mkdir -m 700 "$XDG_RUNTIME_DIR"
fi

# svg FILE R G B: a picture of one colour, larger than the window, so it fills the image area.
svg() {
    printf '<svg xmlns="http://www.w3.org/2000/svg" width="2000" height="2000">' >"$1"
    printf '<rect width="2000" height="2000" fill="rgb(%s,%s,%s)"/></svg>\n' "$2" "$3" "$4" >>"$1"
}

# shows R G B: true once 100,000 pixels of the screen are within 12 of that colour, which an
# icon or a thumbnail is too small for. It waits up to 90 seconds.
shows() {
    python3 -I - "$dir/Xvfb_screen0" "$@" <<'EOF'
import struct, sys, time
path, want = sys.argv[1], [int(v) for v in sys.argv[2:5]]
for _ in range(180):
    data = open(path, "rb").read()
    header = struct.unpack(">25I", data[:100])
    size, width, height, order, bits, line = header[0], header[4], header[5], header[7], header[11], header[12]
    masks, colours = header[14:17], header[19]
    start = size + 12 * colours
    form = "<I" if order == 0 else ">I"
    shifts = [(m & -m).bit_length() - 1 for m in masks]
    seen = 0
    if bits == 32:
        # Every fourth pixel of every fourth row, each counted for 16.
        for y in range(0, height, 4):
            for x in range(0, width, 4):
                (p,) = struct.unpack_from(form, data, start + y * line + x * 4)
                if all(abs(((p & m) >> s) - w) <= 12 for m, s, w in zip(masks, shifts, want)):
                    seen += 16
    if seen >= 100_000:
        sys.exit(0)
    time.sleep(0.5)
print(f"only {seen} pixels of {want}", file=sys.stderr)
sys.exit(1)
EOF
}

# The framebuffer is written to $dir/Xvfb_screen0 as an XWD file while Xvfb runs.
Xvfb :99 -screen 0 1280x1024x24 -fbdir "$dir" -nolisten tcp &
pids+=($!)
export DISPLAY=:99
for _ in $(seq 50); do [ -e "$dir/Xvfb_screen0" ] && break; sleep 0.2; done

svg "$dir/a.svg" 230 30 200
"$@" ${second_window:+--name smoke-first} "$dir/a.svg" &
first=$!
pids+=("$first")
shows 230 30 200 || { echo "FAIL the SVG is not drawn"; exit 1; }
echo "PASS the SVG is drawn"
[ -n "$second_window" ] || exit 0

timeout 30 "$@" --name smoke-first --remote run NewWindow ||
    { echo "FAIL New Window was not taken"; exit 1; }
# A window without --name listens on a socket named for its process ID.
second=
for _ in $(seq 100); do
    second=$(ls "$XDG_RUNTIME_DIR/glimt" 2>/dev/null | sed -n 's/^glimt-\([0-9]*\)\.sock$/\1/p')
    [ -n "$second" ] && break
    sleep 0.3
done
[ -n "$second" ] || { echo "FAIL no second window"; exit 1; }
# The AppImage's runtime runs AppRun in the process it started, so $first is the first window.
# The runtime's mount stays while any process holds its pipe, which the second window
# inherits, so this check, not the drawing below, is what fails when New Window starts the
# binary in the first window's mount instead of the AppImage.
first_program=$(readlink "/proc/$first/exe")
second_program=$(readlink "/proc/$second/exe")
[ "$first_program" != "$second_program" ] ||
    { echo "FAIL the second window runs the first window's $second_program"; exit 1; }
echo "PASS the second window runs a program of its own: $second_program"
timeout 30 "$@" --name smoke-first --remote quit || { echo "FAIL the first window took no quit"; exit 1; }
for _ in $(seq 100); do kill -0 "$first" 2>/dev/null || break; sleep 0.3; done
kill -0 "$first" 2>/dev/null && { echo "FAIL the first window did not close"; exit 1; }
# A new file, which the second window has not drawn before the first one closed. The timeout
# fails the check when no window takes it, as the client would then open a window of its own.
svg "$dir/b.svg" 30 200 90
timeout 30 "$@" --remote open "$dir/b.svg" ||
    { echo "FAIL the second window is gone"; exit 1; }
shows 30 200 90 || { echo "FAIL the second window draws no SVG after the first closed"; exit 1; }
echo "PASS the second window draws an SVG after the first closed"
# The second window is no child of this script: it is closed by remote control.
timeout 30 "$@" --remote-all quit >/dev/null 2>&1
