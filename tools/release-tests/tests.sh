#!/bin/bash
# Release tests, run by run.sh in a rust:1 container with the work folder at /t. As root this
# installs the tools, builds /t/new and /t/old with every feature, starts a virtual X display,
# runs the tests as an ordinary user (so file permissions count) and then cargo audit.
#
# Each check prints PASS or FAIL and only NEW is checked. OLD, the last release, goes through
# the same steps where that tells something: memory and timings, and the bugs NEW fixes.
#
# The clicks and colour probes assume Glimt's default window (1280×800 at the top left) and
# layout; a change to the layout needs the coordinates here changed too. Screenshots of every
# step are in out/, to look at when a check fails.

if [ "$(id -u)" = 0 ]; then
    export DEBIAN_FRONTEND=noninteractive
    echo "-- installing tools"
    # A failed install (an unreachable mirror) stops here: without Xvfb every check would fail.
    apt-get update -qq >/dev/null
    apt-get install -y -qq xvfb xdotool x11-utils imagemagick libimage-exiftool-perl ffmpeg \
        poppler-utils libglib2.0-bin colord-data python3 procps libgl1-mesa-dri libegl1 libgl1 \
        libgl1-mesa-dev libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev libxcursor1 \
        libxrandr2 libxi6 libx11-xcb1 sqlite3 python3-nibabel python3-h5py python3-openslide python3-mrcfile python3-astropy python3-pydicom python3-gdcm libgdcm-tools default-jre-headless unzip >/dev/null || { echo "FAIL installing the tools"; exit 1; }
    # Bio-Formats for its decoder is no Debian package: its release, checked against the hash
    # it had when added.
    curl -sfL -o /tmp/bftools.zip \
        https://downloads.openmicroscopy.org/bio-formats/8.5.0/artifacts/bftools.zip
    echo "07a3bb1d3de84da3a709655a1008cb2d9b19becc5bad4ae4112633aec9380478  /tmp/bftools.zip" \
        | sha256sum -c --quiet && (cd /opt && unzip -q /tmp/bftools.zip) \
        || echo "FAIL installing bftools"
    # tools/screenshots/run.sh (SHOTS=1) needs only this version.
    for v in new $([ -z "$SHOTS" ] && echo old); do
        echo "-- building $v"
        (cd /t/$v && CARGO_TARGET_DIR=/work-target/release-tests-$v \
            cargo build -q --release --locked --features full) || exit 1
        cp /work-target/release-tests-$v/release/glimt /opt/glimt-$v
    done
    useradd -m tester
    chmod 777 /t/out
    su tester -c "Xvfb :98 -screen 0 1600x1000x24 >/tmp/xvfb.log 2>&1 &"
    sleep 2
    su tester -c "QUICK=$QUICK SHOTS=$SHOTS bash /t/tests.sh"
    status=$?
    [ -n "$SHOTS" ] && exit $status
    echo "-- cargo audit"
    tools=/usr/local/cargo/registry/tools
    [ -x $tools/bin/cargo-audit ] || cargo install -q --locked --root $tools cargo-audit
    if (set -o pipefail; cd /t/new && $tools/bin/cargo-audit audit 2>&1 | tail -12); then
        echo "PASS cargo audit: no known vulnerabilities"
    else
        echo "FAIL cargo audit: a crate with a known vulnerability"
        status=1
    fi
    exit $status
fi

# ---------------------------------------------------------------- as the user "tester"

export DISPLAY=:98 LIBGL_ALWAYS_SOFTWARE=1
FAILED=0
pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; FAILED=$((FAILED + 1)); }
# expect NAME GOT WANT
expect() { if [ "$2" = "$3" ]; then pass "$1"; else fail "$1: got '$2', want '$3'"; fi; }
note() { echo "     $*"; }

# The environment of one version, new or old: a fresh HOME and XDG folders, and a stand-in for
# zenity. Sets V, BIN and H.
as_version() {
    export V=$1 BIN=/opt/glimt-$1 H=/tmp/home-$1
    export HOME=$H XDG_CACHE_HOME=$H/.cache XDG_CONFIG_HOME=$H/.config XDG_RUNTIME_DIR=$H/run \
        XDG_STATE_HOME=$H/.local/state XDG_DATA_HOME=$H/.local/share
    mkdir -p $XDG_CACHE_HOME $XDG_CONFIG_HOME/glimt $XDG_RUNTIME_DIR /tmp/stub
    chmod 700 $XDG_RUNTIME_DIR
    export PATH=/tmp/stub:/usr/local/bin:/usr/bin:/bin
}
config() { printf '%s\n' "$@" >$XDG_CONFIG_HOME/glimt/config; }
# The file or folder the stand-in file dialog answers with.
answer() { printf '#!/bin/sh\necho "%s"\n' "$1" >/tmp/stub/zenity; chmod +x /tmp/stub/zenity; }
# Start Glimt on ARGS and wait for its window; a second try if X refused the first. Sets GPID
# (its process) and WIN (its window), which key, quit and the screenshots use.
start() {
    for try in 1 2; do
        $BIN "$@" 2>/tmp/glimt.err &
        GPID=$!
        for i in $(seq 100); do
            WIN=$(xdotool search --pid $GPID 2>/dev/null | tail -1)
            [ -n "$WIN" ] && break
            kill -0 $GPID 2>/dev/null || break
            sleep 0.2
        done
        [ -n "$WIN" ] && break
        sleep 2
    done
    # Without a window every check that expects nothing to change would pass. The last release
    # may lack an option a new check uses: noted only.
    if [ -z "$WIN" ]; then
        if [ "$V" = new ]; then fail "no window: $*"; else note "old: no window: $*"; fi
    fi
    sleep 1.5
    xdotool windowfocus $WIN 2>/dev/null
    xdotool mousemove 800 400 2>/dev/null
}
key() { xdotool windowfocus $WIN 2>/dev/null; xdotool key "$@" 2>/dev/null; sleep 0.6; }
click() { xdotool mousemove $1 $2 click 1 2>/dev/null; sleep ${3:-1}; }
quit() {
    $BIN --remote-all quit >/dev/null 2>&1
    sleep 1
    kill $GPID 2>/dev/null
    wait $GPID 2>/dev/null
}
shot() { import -window root /t/out/$1.png; }
# The colour at X,Y of a screenshot: red, green, blue, yellow, magenta, cyan or other.
colour() {
    pixel $1 $2 $3 | {
        read r g b
        hi() { [ $1 -ge 110 ]; }
        lo() { [ $1 -le 90 ]; }
        if hi $r && lo $g && lo $b; then echo red
        elif lo $r && hi $g && lo $b; then echo green
        elif lo $r && lo $g && hi $b; then echo blue
        elif hi $r && hi $g && lo $b; then echo yellow
        elif hi $r && lo $g && hi $b; then echo magenta
        elif lo $r && hi $g && hi $b; then echo cyan
        else echo other; fi
    }
}
# Red or blue at X,Y of an image file, for the pictures that are half red, half blue.
side() { [ "$(convert "$1" -format "%[fx:p{$2}.r>0.5]" info:)" = 1 ] && echo red || echo blue; }
# Rows of the file list with thumbnails, at x 57.
ROWS=(337 419 501 583 665)
rows() { for y in "${ROWS[@]:0:$2}"; do colour $1 57 $y; done | tr '\n' ' ' | sed 's/ $//'; }
# The colour at X,Y of a screenshot as "r g b", 0-255 each.
pixel() {
    convert /t/out/$1.png -format "%[fx:int(255*p{$2,$3}.r)] %[fx:int(255*p{$2,$3}.g)] \
%[fx:int(255*p{$2,$3}.b)]" info:
}
# differ NAME A B: a check that A and B are not the same.
differ() { if [ "$2" != "$3" ]; then pass "$1"; else fail "$1: both '$2'"; fi; }
# The red value, 0-255, at X,Y of a screenshot.
red() { convert /t/out/$1.png -format "%[fx:int(255*p{$2,$3}.r)]" info:; }
first_image() { grep -o 'first image: [0-9]* ms' /tmp/glimt.err; }
wait_first() { for i in $(seq 240); do grep -q "first image" /tmp/glimt.err && break; sleep 0.5; done; }
rss() { awk '/RssAnon/ {printf "%d MB", $2 / 1024}' /proc/$GPID/status; }
# The smaller of the variable named $1 and the number $2, kept in $1: the best of the runs, as one
# can be slow by chance. within: yes when $1 is at most 1.5 times $2, plus the slack $3 (ms or
# MB): a few hundred ms vary by more than half when something else runs on the machine.
best() { eval "best_seen=\${$1:-}"; { [ -z "$best_seen" ] || [ "${2:-999999}" -lt "$best_seen" ]; } && eval "$1=${2:-999999}"; }
within() { [ $(( ${1:-999999} * 2 )) -le $(( ${2:-0} * 3 + ${3:-0} * 2 )) ] && echo yes; }
hwm() { awk '/VmHWM/ {print $2}' /proc/$GPID/status; }

cd /tmp

# tools/screenshots/run.sh: the pictures for the README and the website, instead of the checks.
if [ -n "$SHOTS" ]; then
    as_version new
    . /t/shots.sh
    exit $FAILED
fi

echo "-- saving turned: PNG, lossless WebP and lossy WebP with EXIF orientation 6"
as_version new
for rotate in true false; do
    d=/tmp/turned-$rotate
    mkdir -p $d && cd $d
    convert -size 200x200 xc:red -size 200x200 xc:blue +append -depth 8 a.png
    convert a.png -define webp:lossless=true b.webp
    convert a.png -quality 90 c.webp
    exiftool -q -q -overwrite_original -Orientation#=6 -Make=Canon a.png b.webp c.webp
    cd /tmp
    config save_orientation=true exif_rotate=$rotate
    start $d
    for f in a b c; do key bracketright; key space; done
    sleep 6
    quit
    for f in a.png b.webp c.webp; do
        p() { side $d/$f $1; }
        size=$(identify -format '%wx%h' $d/$f)
        # Turned upright by EXIF and then a quarter turn: blue left; without EXIF, red on top.
        if [ $rotate = true ]; then got="$size $(p 10,100) $(p 390,100)"; want="400x200 blue red"
        else got="$size $(p 100,10) $(p 100,390)"; want="200x400 red blue"; fi
        expect "$f, Auto Rotate $rotate: pixels as shown" "$got" "$want"
        expect "$f, Auto Rotate $rotate: EXIF kept, upright" \
            "$(exiftool -s3 -n -Orientation -Make $d/$f | tr '\n' ' ')" "1 Canon "
    done
done

echo "-- saves at once: a.png and a.webp of 6000x4000, turned one after the other"
d=/tmp/at-once
mkdir -p $d && cd $d
convert -size 3000x4000 xc:red -size 3000x4000 xc:blue +append a.png
convert a.png -define webp:lossless=true a.webp
cd /tmp
config save_orientation=true
start $d
key bracketright; key space; key bracketright; key space
for i in $(seq 60); do sleep 1; ls $d | grep -q glimt-tmp || break; done
sleep 2
quit
for f in a.png a.webp; do
    expect "$f: whole and turned" \
        "$(identify -format '%wx%h' $d/$f) $(side $d/$f 2000,10) $(side $d/$f 2000,5990)" \
        "4000x6000 red blue"
done
expect "no temporary files left" "$(ls $d | tr '\n' ' ')" "a.png a.webp "

echo "-- a turn saved when another folder opens: to the turned image, not the new folder's"
for v in new old; do
    as_version $v
    d=/tmp/turn-open-$v
    mkdir -p $d/one $d/two
    convert -size 200x200 xc:red -size 200x200 xc:blue +append -depth 8 $d/one/a.png
    for f in one/b two/c two/d; do cp $d/one/a.png $d/$f.png; done
    config save_orientation=true
    start $d/one/b.png
    key bracketright
    $BIN --remote-all open $d/two >/dev/null 2>&1
    sleep 4
    quit
    got="$(cd $d && identify -format '%wx%h ' one/a.png one/b.png two/c.png two/d.png)"
    if [ $v = new ]; then
        expect "one/b.png turned, the rest as they were" "$got" "400x200 200x400 400x200 400x200 "
    else note "old: one/a one/b two/c two/d: $got"; fi
done
as_version new

echo "-- a JPEG turned and left: its orientation field saved, in the background"
d=/tmp/turn-jpeg
mkdir -p $d
convert -size 200x100 xc:red $d/a.jpg
cp $d/a.jpg $d/b.jpg
config save_orientation=true
start $d/a.jpg
key bracketright; key space; sleep 2      # turned clockwise, then the next image
quit
got="$(exiftool -s3 -n -Orientation $d/a.jpg) $(exiftool -s3 -n -Orientation $d/b.jpg)"
expect "a.jpg has orientation 6, b.jpg none" "$got" "6 "

echo "-- Copy to Folder, then Move to Folder while its dialog is open: the copy is done, not a move"
for v in new old; do
    as_version $v
    d=/tmp/two-dialogs-$v
    mkdir -p $d/sub
    convert -size 100x100 xc:red $d/a.jpg
    printf '#!/bin/sh\nsleep 3\necho "%s"\n' $d/sub >/tmp/stub/zenity; chmod +x /tmp/stub/zenity
    config auto_refresh=false
    start $d/a.jpg
    key ctrl+c; key ctrl+m; sleep 5
    quit
    got="$(ls $d | tr '\n' ' ')$(ls $d/sub | tr '\n' ' ')"
    if [ $v = new ]; then expect "a.jpg copied, still in its folder" "$got" "a.jpg sub a.jpg "
    else note "old: $got"; fi
done
as_version new

echo "-- a turned image removed by another program: its turn goes to no other image"
for v in new old; do
    as_version $v
    d=/tmp/turn-gone-$v
    mkdir -p $d
    convert -size 200x200 xc:red -size 200x200 xc:blue +append -depth 8 $d/a.png
    # b.png green and yellow: shown unturned after a.png goes, its left half green at 400 410.
    convert -size 200x200 xc:lime -size 200x200 xc:yellow +append -depth 8 $d/b.png
    config save_orientation=true auto_refresh=true
    start $d/a.png
    sleep 2
    key bracketright                 # turned, not saved yet
    rm $d/a.png; sleep 3             # gone, and the folder read again
    shot turn-gone-$v
    key space; sleep 2               # moving on saves a turn, if one is kept
    quit
    got="$(identify -format '%wx%h' $d/b.png)"
    shown=$(colour turn-gone-$v 400 410)
    if [ $v = new ]; then
        expect "b.png as it was" "$got" "400x200"
        expect "b.png shown unturned" "$shown" green
    else note "old: b.png $got, shown $shown"; fi
done
as_version new

echo "-- subfolders, auto refresh off: copy and move to the folder and a subfolder"
d=/tmp/subfolders
mkdir -p $d/sub/deep && cd $d
convert -size 300x200 xc:red a.jpg
convert -size 300x200 xc:lime b.jpg
convert -size 300x200 xc:blue sub/c.jpg
convert -size 300x200 xc:yellow sub/d.jpg
cd /tmp
config show_subfolders=true auto_refresh=false
start $d
answer $d/sub; key ctrl+c; sleep 2
key space; key space; key space; answer $d; key ctrl+m; sleep 2
key Home; key space; answer $d/sub/deep; key ctrl+m; sleep 2
shot subfolders
quit
expect "files on disk" "$(cd $d && find . -name '*.jpg' | sort | tr '\n' ' ')" \
    "./a.jpg ./c.jpg ./sub/a.jpg ./sub/d.jpg ./sub/deep/b.jpg "
# a.jpg, c.jpg, sub/a.jpg, sub/d.jpg, sub/deep/b.jpg
expect "the list follows" "$(rows subfolders 5)" "red blue red yellow green"

echo "-- a delete that fails in a subfolder: the file comes back"
for v in new old; do
    as_version $v
    d=/tmp/failed-delete-$v
    mkdir -p $d/sub && cd $d
    convert -size 300x200 xc:blue a.jpg
    convert -size 300x200 xc:red sub/b.jpg
    convert -size 300x200 xc:lime sub/c.jpg
    chmod 555 sub
    cd /tmp
    config show_subfolders=true auto_refresh=false confirm_delete=false
    start $d
    key space; key shift+Delete; sleep 3
    shot failed-delete-$v
    # A delete that can succeed, so the check above cannot pass by nothing happening.
    key Home; key shift+Delete; sleep 2
    quit
    chmod 755 $d/sub
    # a.jpg, sub/b.jpg (not deleted), sub/c.jpg
    if [ $v = new ]; then
        expect "the file is in the list again" "$(rows failed-delete-new 3)" "blue red green"
        expect "a file that can go is deleted" "$(ls $d)" "sub"
    else note "old: $(rows failed-delete-old 3)"; fi
done
as_version new

echo "-- a delete that fails in a collection: the image stays in the collection"
for v in new old; do
    as_version $v
    d=/tmp/failed-collection-delete-$v
    mkdir -p $d/ro
    convert -size 300x200 xc:blue $d/a.jpg
    convert -size 300x200 xc:red $d/ro/b.jpg
    chmod 555 $d/ro
    printf '#GQview collection\n"%s/a.jpg"\n"%s/ro/b.jpg"\n' $d $d >$d/show.gqv
    config auto_refresh=false confirm_delete=false
    start $d/show.gqv
    sleep 2
    key End; key shift+Delete; sleep 3     # ro/b.jpg cannot be deleted
    # One that can go: the check above cannot pass by nothing happening.
    key Home; key shift+Delete; sleep 2
    quit
    chmod 755 $d/ro
    got=$(grep -c 'ro/b.jpg' $d/show.gqv)
    if [ $v = new ]; then
        expect "b.jpg still in the collection" "$got" 1
        expect "a.jpg deleted" "$([ -e $d/a.jpg ] || echo gone)" gone
    else note "old: b.jpg lines in the collection: $got"; fi
done
as_version new

echo "-- a file of the same name opened in another folder: not at the zoom of the one before"
for v in new old; do
    as_version $v
    d=/tmp/same-name-$v
    mkdir -p $d/one $d/two
    convert -size 400x300 xc:blue -fill red -draw 'rectangle 100,75 300,225' $d/one/a.png
    cp $d/one/a.png $d/two/a.png
    config
    start $d/one/a.png
    sleep 2
    key equal; key equal; sleep 1
    $BIN --remote open $d/two/a.png >/dev/null 2>&1; sleep 2
    shot same-name-$v
    quit
    got=$(colour same-name-$v 400 410)
    if [ $v = new ]; then expect "fitted, not zoomed" "$got" blue
    else note "old: $got"; fi
done
as_version new

echo "-- the folder read again while zoomed in: the zoom stays"
for v in new old; do
    as_version $v
    d=/tmp/refresh-zoom-$v
    mkdir -p $d
    # Blue with a red middle: zoomed in, red reaches the left of the image area.
    convert -size 400x300 xc:blue -fill red -draw 'rectangle 100,75 300,225' $d/a.png
    config auto_refresh=true
    start $d/a.png
    sleep 2
    key equal; key equal; sleep 1
    convert -size 64x64 xc:lime $d/z.png      # another program writes to the folder
    sleep 3
    shot refresh-zoom-$v
    quit
    got=$(colour refresh-zoom-$v 400 410)
    if [ $v = new ]; then expect "still zoomed in" "$got" red
    else note "old: $got"; fi
done
as_version new

echo "-- a refresh keeps the selection: a delete after it takes what was selected"
for v in new old; do
    as_version $v
    d=/tmp/refresh-selection-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    config confirm_delete=false
    start $d
    # a and b selected, then a file comes from elsewhere and auto refresh reads the folder.
    click 150 337; xdotool keydown ctrl; click 150 419; xdotool keyup ctrl
    cp $d/c.jpg $d/new.jpg; sleep 3
    shot refresh-selection-$v
    key shift+Delete; sleep 2
    quit
    left=$(ls $d | tr '\n' ' ')
    if [ $v = new ]; then expect "the selected files deleted" "$left" "c.jpg new.jpg "
    else note "old, left: $left"; fi
done
as_version new

echo "-- --remote current and selection: the paths, each ending in NUL, also with a line break"
for v in new old; do
    as_version $v
    d=/tmp/remote-query-$v
    mkdir -p $d
    for c in a b; do convert -size 300x200 xc:red $d/$c.jpg; done
    convert -size 300x200 xc:red "$d/line
break.jpg"
    config auto_refresh=false
    start $d/a.jpg
    sleep 1
    current=$($BIN --remote current | tr '\0' '|')
    click 150 337; xdotool keydown ctrl; click 150 419; xdotool keyup ctrl
    selection=$($BIN --remote selection | tr '\0' '|')
    # The name with a line break, shown: its path read whole, the file found by it.
    key End; sleep 1
    size=$($BIN --remote current | xargs -0 stat -c %s 2>/dev/null)
    mode=$(stat -c %a $XDG_RUNTIME_DIR/glimt)
    quit
    if [ $v = new ]; then
        expect "current: the shown image" "$current" "$d/a.jpg|"
        expect "selection: both selected" "$selection" "$d/a.jpg|$d/b.jpg|"
        expect "a name with a line break comes whole" "$size" "$(stat -c %s "$d/line
break.jpg")"
        expect "the sockets' folder only for the user" "$mode" 700
    else note "old: current '$current', folder $mode"; fi
done
as_version new

echo "-- --remote select while Delete asks: the answer deletes the file it asked about only"
for v in new old; do
    as_version $v
    d=/tmp/select-dialog-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    config confirm_delete=true
    start $d
    sleep 2
    key shift+Delete; sleep 1           # "Permanently delete a.png?"
    $BIN --remote select $d/a.png $d/b.png $d/c.png >/dev/null 2>&1 &
    sleep 1
    key Return; sleep 2
    quit
    got="$(ls $d | tr '\n' ' ')"
    if [ $v = new ]; then expect "only a deleted" "$got" "b.png c.png "
    else note "old, left: $got"; fi
done
as_version new

echo "-- --remote open with a line break in the name: refused, not read as a second command"
for v in new old; do
    as_version $v
    d=/tmp/remote-newline-$v
    mkdir -p $d
    convert -size 64x64 xc:red $d/a.png
    config
    start $d
    sleep 2
    $BIN --remote open "$d/x
quit" >/dev/null 2>&1; refused=$?
    sleep 1
    running=$(kill -0 $GPID 2>/dev/null && echo running || echo quit)
    quit
    if [ $v = new ]; then expect "refused, Glimt still running" "$refused $running" "2 running"
    else note "old: $refused $running"; fi
done
as_version new

echo "-- --remote select while Preferences is open: answered at once"
for v in new old; do
    as_version $v
    d=/tmp/select-prefs-$v
    mkdir -p $d
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d
    sleep 2
    key ctrl+o; sleep 1                    # Preferences
    start_at=$(date +%s)
    $BIN --remote select $d/b.png >/dev/null 2>&1
    took=$(( $(date +%s) - start_at ))
    got=$($BIN --remote selection 2>/dev/null | tr '\0' ' ' | sed "s|$d/||g")
    quit
    if [ $v = new ]; then expect "b selected at once" "$got $([ $took -lt 5 ] && echo soon)" "b.png  soon"
    else note "old: '$got', ${took}s"; fi
done
as_version new

echo "-- --remote select through a linked folder: the files are found"
for v in new old; do
    as_version $v
    d=/tmp/select-link-$v
    mkdir -p $d/real
    for f in a b; do convert -size 64x64 xc:red $d/real/$f.png; done
    ln -sfn $d/real $d/link
    ln -sf a.png $d/real/c.png            # a linked image, listed under its own name
    config
    start $d/link
    sleep 2
    missing=$($BIN --remote select $d/link/b.png $d/link/c.png 2>/dev/null | tr '\0' ' ')
    got=$($BIN --remote selection 2>/dev/null | tr '\0' ' ' | sed "s|$d/real/||g")
    quit
    if [ $v = new ]; then expect "b and the link c selected, none missing" "$got|$missing" "b.png c.png |"
    else note "old: $got|$missing"; fi
done
as_version new

echo "-- --remote select: right after open, files of the list selected, the others printed"
for v in new old; do
    as_version $v
    d=/tmp/remote-select-$v
    mkdir -p $d/first
    for c in a b "c d"; do convert -size 300x200 xc:red "$d/$c.jpg"; done
    convert -size 300x200 xc:red $d/first/x.jpg
    config auto_refresh=false
    start $d/first
    sleep 1
    # Sent at once after open, while the folder is still being read.
    $BIN --remote open $d
    missing=$($BIN --remote select $d/b.jpg "$d/c d.jpg" /nowhere/e.jpg | tr '\0' '|')
    sleep 1
    selection=$($BIN --remote selection | tr '\0' '|')
    current=$($BIN --remote current | tr '\0' '|')
    quit
    if [ $v = new ]; then
        expect "select: the file not in the list printed" "$missing" "/nowhere/e.jpg|"
        expect "select: the others selected" "$selection" "$d/b.jpg|$d/c d.jpg|"
        expect "select: the first shown" "$current" "$d/b.jpg|"
    else note "old: '$missing' '$selection'"; fi
done
as_version new

echo "-- copying 120 files of 25 MB, closed halfway: counted in the status bar, every copy whole"
for v in new old; do
    as_version $v
    d=/tmp/progress-$v
    mkdir -p $d/to
    head -c 25000000 /dev/urandom >$d/00.jpg
    for i in $(seq -w 1 119); do cp $d/00.jpg $d/$i.jpg; done
    config
    start $d
    sleep 2
    key ctrl+a
    answer $d/to
    key ctrl+c
    # The copy starts once the stand-in file dialog has answered.
    for i in $(seq 100); do [ -n "$(ls -A $d/to)" ] && break; sleep 0.1; done
    sleep 0.5
    shot progress-$v
    key ctrl+q; sleep 1
    waited=$(kill -0 $GPID 2>/dev/null && echo waits || echo closed)
    for i in $(seq 240); do kill -0 $GPID 2>/dev/null || break; sleep 0.5; done
    quit
    whole=$(find $d/to -name '*.jpg' -size 24415k | wc -l)
    parts=$(ls -A $d/to | grep -c glimt-copy)
    rm -rf $d    # 6 GB
    if [ $v = new ]; then
        expect "the window waits for the copies" "$waited" waits
        expect "all 120 copies whole, no hidden ones left" "$whole $parts" "120 0"
    else note "old: $waited, $whole whole, $parts hidden"; fi
done
as_version new

echo "-- copying, closed twice: stopped after the file at hand, no hidden half copy left"
for v in new old; do
    as_version $v
    d=/tmp/close-twice-$v
    mkdir -p $d/to
    head -c 25000000 /dev/urandom >$d/00.jpg
    for i in $(seq -w 1 119); do cp $d/00.jpg $d/$i.jpg; done
    config
    start $d
    sleep 2
    key ctrl+a
    answer $d/to
    key ctrl+c
    for i in $(seq 100); do [ -n "$(ls -A $d/to)" ] && break; sleep 0.1; done
    sleep 0.5
    key ctrl+q; key ctrl+q
    for i in $(seq 120); do kill -0 $GPID 2>/dev/null || break; sleep 0.5; done
    quit
    parts=$(ls -A $d/to | grep -c glimt-copy)
    copied=$(ls $d/to | grep -c jpg)
    whole=$(find $d/to -name '*.jpg' -size 24415k | wc -l)
    rm -rf $d    # 6 GB
    if [ $v = new ]; then
        expect "no hidden half copy, every copy whole" "$parts $([ $copied = $whole ] && echo whole)" "0 whole"
        expect "stopped before all 120" "$([ $copied -lt 120 ] && echo yes)" yes
    else note "old: $parts hidden, $copied copied, $whole whole"; fi
done
as_version new

echo "-- thumbnails with Auto Rotate off"
rm -rf $XDG_CACHE_HOME/thumbnails
config exif_rotate=false
start /tmp/subfolders
sleep 3
quit
expect "none saved to the shared cache" "$(find $XDG_CACHE_HOME/thumbnails -type f 2>/dev/null | wc -l)" "0"
# With it on they are saved: the check above cannot pass by no thumbnails being made.
config exif_rotate=true
start /tmp/subfolders
sleep 3
quit
n=$(find $XDG_CACHE_HOME/thumbnails -type f 2>/dev/null | wc -l)
[ $n -gt 0 ] && pass "with Auto Rotate on, $n saved" || fail "with Auto Rotate on, none saved"

echo "-- stars follow a renamed file, and go with a deleted one"
for v in new old; do
    as_version $v
    d=/tmp/stars-follow-$v
    mkdir -p $d $XDG_STATE_HOME/glimt
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    printf '0\t4\t%s/a.png\n0\t2\t%s/b.png\n' $d $d >$XDG_STATE_HOME/glimt/marks
    config confirm_delete=false
    start $d
    sleep 1
    key ctrl+r; sleep 1
    key ctrl+a
    xdotool type x
    key Return; sleep 1
    key Home; key shift+Delete; sleep 2     # b, first after the rename
    quit
    got="$(sort $XDG_STATE_HOME/glimt/marks | tr '\t\n' '  ')"
    if [ $v = new ]; then expect "the four stars on x, none left for b" "$got" "0 4 $d/x "
    else note "old: $got"; fi
done
as_version new

echo "-- stars that cannot be saved (a folder that cannot be written): told, not lost silently"
for v in new old; do
    as_version $v
    d=/tmp/marks-error-$v
    mkdir -p $d $XDG_STATE_HOME/glimt
    convert -size 64x64 xc:red $d/a.png
    chmod 555 $XDG_STATE_HOME/glimt
    config
    start $d
    sleep 2
    key alt+3; sleep 2                  # three stars
    told=$(grep -c "Could not save .*marks" /tmp/glimt.err)
    quit
    chmod 755 $XDG_STATE_HOME/glimt
    if [ $v = new ]; then expect "the failed save told" "$told" 1
    else note "old: $told"; fi
done
as_version new

echo "-- a mark set by another window meanwhile: a mark's key here goes from it"
for v in new old; do
    as_version $v
    d=/tmp/marks-windows-$v
    mkdir -p $d $XDG_STATE_HOME/glimt
    convert -size 64x64 xc:red $d/a.png
    rm -f $XDG_STATE_HOME/glimt/marks
    config
    start $d
    sleep 2
    # As if marked in another window: mark 1 on a.png, which this one has not seen.
    printf '1\t0\t%s/a.png\n' $d >$XDG_STATE_HOME/glimt/marks
    key 1; sleep 2                       # mark 1 toggled: off, as it was on
    got=$(grep -c "a.png" $XDG_STATE_HOME/glimt/marks)
    quit
    if [ $v = new ]; then expect "the mark taken off" "$got" 0
    else note "old: $got"; fi
done
as_version new

echo "-- stars to XMP for two files, the first one's sidecar not writable: the second still gets them"
for v in new old; do
    as_version $v
    d=/tmp/stars-xmp-$v
    mkdir -p $d/a.png.xmp                # where a's sidecar would go: a folder, so it fails
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    config xmp_stars=true
    start $d
    sleep 2
    key ctrl+a; key alt+3; sleep 3
    got=$(grep -c 'Rating="3"\|Rating>3<' $d/b.png.xmp 2>/dev/null)
    quit
    if [ $v = new ]; then expect "b's sidecar has three stars" "$got" 1
    else note "old: ${got:-none}"; fi
done
as_version new

echo "-- Rename answered after the folder was read again: the same file renamed"
for v in new old; do
    as_version $v
    d=/tmp/rename-refresh-$v
    mkdir -p $d
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    config auto_refresh=true
    start $d
    key ctrl+r
    convert -size 64x64 xc:blue $d/z.png      # another program writes to the folder
    sleep 3                                   # read again meanwhile
    key ctrl+a
    xdotool type x
    key Return
    quit
    got="$(ls $d | tr '\n' ' ')"
    if [ $v = new ]; then expect "a.png renamed to x" "$got" "b.png x z.png "
    else note "old: $got"; fi
done
as_version new

echo "-- Rename with one file selected by a right-click: that file renamed, not the one shown"
for v in new old; do
    as_version $v
    d=/tmp/rename-selected-$v
    mkdir -p $d
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d/a.png
    sleep 2
    xdotool mousemove 120 419 click 3; sleep 1   # b.png's row: selected, its menu opens
    key Escape
    key ctrl+r; sleep 1
    key ctrl+a
    xdotool type x
    key Return
    quit
    got="$(ls $d | tr '\n' ' ')"
    if [ $v = new ]; then expect "b.png renamed to x" "$got" "a.png x "
    else note "old: $got"; fi
done
as_version new

echo "-- Rename answered after --remote next: the file asked about renamed, not the next image"
for v in new old; do
    as_version $v
    d=/tmp/rename-next-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d
    key ctrl+r
    $BIN --remote-all next >/dev/null 2>&1
    sleep 1
    key ctrl+a
    xdotool type x
    key Return
    quit
    got="$(ls $d | tr '\n' ' ')"
    if [ $v = new ]; then expect "a.png renamed, the others kept" "$got" "b.png c.png x "
    else note "old: $got"; fi
done
as_version new
# The same Rename without the step renames: the check above cannot pass by no dialog opening.
d=/tmp/rename-plain
mkdir -p $d
for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
config
start $d
key ctrl+r; sleep 1; key ctrl+a
xdotool type x
key Return
quit
expect "a.png renamed to x" "$(ls $d | tr '\n' ' ')" "b.png c.png x "

echo "-- Rename Many of 20,000 in a collection, an image taken out while it runs: the others stay"
for v in new old; do
    as_version $v
    d=/tmp/rename-collection-$v
    mkdir -p $d
    convert -size 64x64 xc:red $d/00000.jpg
    for i in $(seq -w 1 19999); do ln $d/00000.jpg $d/$i.jpg; done
    printf '#GQview collection\n' >$d/show.gqv
    for f in $(ls $d/*.jpg); do printf '"%s"\n' $f >>$d/show.gqv; done
    config
    start $d/show.gqv
    sleep 3
    key ctrl+a; key ctrl+r; sleep 1
    # Rename Many as *_###, in the background, and at once, with nothing selected, the last
    # image out of the collection.
    key Return ctrl+shift+a End Delete
    sleep 8
    quit
    kept=$(grep -c '_[0-9]*\.jpg' $d/show.gqv)
    if [ $v = new ]; then expect "the renamed images in the collection" "$([ $kept -ge 19998 ] && echo yes)" yes; fi
    note "$v: $kept renamed in the collection"
done
as_version new

echo "-- Rename Many failing halfway: names that swap, and one too long"
d=/tmp/rename-many
long=$(printf 'b%.0s' $(seq 251))
mkdir -p $d && cd $d
for f in 1 a a2 $long; do convert -size 100x100 xc:blue $f.jpg; done
cd /tmp
config
start $d
key ctrl+a; key ctrl+r; sleep 1; key ctrl+a
xdotool type --delay 30 '*#'; sleep 0.4
key Return; sleep 2
quit
# 1 -> 11, a -> a2 (a name in use: temporary names first), a2 -> a23, the long one too long.
expect "every name kept" "$(ls -A $d | sed "s/$long/LONG/" | tr '\n' ' ')" "1.jpg a.jpg a2.jpg LONG.jpg "
# Without the long one it renames: the check above cannot pass by nothing happening.
rm $d/$long.jpg
start $d
key ctrl+a; key ctrl+r; sleep 1; key ctrl+a
xdotool type --delay 30 '*#'; sleep 0.4
key Return; sleep 2
quit
expect "renamed with the names swapped" "$(ls -A $d | tr '\n' ' ')" "11.jpg a2.jpg a23.jpg "

echo "-- Rename Many with Include Subfolders: the view stays on the folder and its subfolders"
for v in new old; do
    as_version $v
    d=/tmp/rename-subfolders-$v
    mkdir -p $d/sub
    convert -size 100x100 xc:blue $d/c.jpg
    convert -size 100x100 xc:red $d/sub/a.jpg
    convert -size 100x100 xc:lime $d/sub/b.jpg
    config show_subfolders=true auto_refresh=false
    start $d
    sleep 2
    key End                       # sub/b.jpg shown, so it is among the renamed
    key ctrl+a; key ctrl+r; sleep 1; key ctrl+a
    xdotool type --delay 30 'x*'; sleep 0.4
    key Return; sleep 3
    key End; sleep 1              # the folder's own file sorts last, after sub/
    got=$($BIN --remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    quit
    if [ $v = new ]; then expect "the folder's own file still in the list" "$got" "xc.jpg"
    else note "old, last: $got"; fi
done
as_version new

echo "-- Find Duplicates: trash the checked files"
for v in new old; do
    as_version $v
    d=/tmp/duplicates-$v
    rm -rf $H/.local/share/Trash
    mkdir -p $d/other && cd $d
    convert -size 300x200 xc:red a.jpg; cp a.jpg b.jpg; cp a.jpg c.jpg
    convert -size 300x200 xc:lime other/x.jpg
    cd /tmp
    config confirm_trash=false
    # Checked, then another folder opened while the window is open: nothing may be deleted.
    start $d
    # D opens Find Duplicates; then Find, and "Check all but the first of each group".
    key d; click 525 82 2; click 147 129
    $BIN --remote open $d/other/x.jpg; sleep 2
    click 459 129 3   # "Move 2 checked to trash…"
    shot duplicates-$v
    quit
    trash=$(ls $H/.local/share/Trash/files 2>/dev/null | tr '\n' ' ')
    if [ $v = new ]; then expect "the list changed: nothing in the trash" "$trash" ""
    else note "old, in the trash: $trash"; fi
done
as_version new
rm -rf $H/.local/share/Trash
start /tmp/duplicates-new
key d; click 525 82 2; click 147 129; click 459 129 3
quit
expect "the list as it was: the checked in the trash" \
    "$(ls $H/.local/share/Trash/files 2>/dev/null | tr '\n' ' ')" "b.jpg c.jpg "

echo "-- pan view: another sort order keeps each date's image"
for v in new old; do
    as_version $v
    d=/tmp/pan-$v
    mkdir -p $d
    n=1
    for c in red lime blue yellow magenta cyan; do
        convert -size 300x200 xc:$c $d/$c.jpg
        touch -d "2024-01-0$n 12:00" $d/$c.jpg
        n=$((n + 1))
    done
    config
    start $d
    key ctrl+j; sleep 2
    click 60 787; click 55 581 3   # the sort menu in the status bar: Sort by date
    shot pan-$v
    quit
    got="$(colour pan-$v 80 173) $(colour pan-$v 80 297) $(colour pan-$v 80 545)"
    if [ $v = new ]; then expect "2024-01-01, -02 and -04" "$got" "red green yellow"
    else note "old: $got"; fi
done

echo "-- pan view far from the file list's rows: its cells get thumbnails"
convert -size 300x200 xc:gray /tmp/gray.jpg
for v in new old; do
    as_version $v
    d=/tmp/pan-far-$v
    mkdir -p $d
    # By name n00 is first in the list; by date n59 is first in pan view, 59 files away.
    for i in $(seq -w 0 59); do cp /tmp/gray.jpg $d/n$i.jpg; done
    convert -size 300x200 xc:red $d/n59.jpg
    convert -size 300x200 xc:lime $d/n58.jpg
    for i in $(seq 0 59); do
        touch -d "2024-01-01 + $((59 - i)) days 12:00" $d/n$(printf %02d $i).jpg
    done
    config
    start $d
    key ctrl+j; sleep 4
    shot pan-far-$v
    quit
    got="$(colour pan-far-$v 80 173) $(colour pan-far-$v 80 297)"
    if [ $v = new ]; then expect "n59 and n58 shown in pan view" "$got" "red green"
    else note "old: $got"; fi
done

echo "-- a video that is a playlist: ffmpeg opens no file it names"
for v in new old; do
    as_version $v
    d=/tmp/playlist-$v
    mkdir -p $d/s
    ffmpeg -v error -f lavfi -i color=c=blue:s=320x240:d=2 -pix_fmt yuv420p $d/real.mp4
    convert -size 300x200 xc:red $d/s/secret.png
    # By its content a concat list, which ffmpeg would follow to s/secret.png.
    printf 'ffconcat version 1.0\nfile s/secret.png\n' >$d/fake.mp4
    config
    start $d/real.mp4
    sleep 3
    shot playlist-real-$v
    key BackSpace; sleep 3   # fake.mp4, before real.mp4
    shot playlist-fake-$v
    quit
    # Beside the play button in the middle.
    got="$(colour playlist-real-$v 500 250) $(colour playlist-fake-$v 500 250)"
    if [ $v = new ]; then expect "a real video shown, the playlist not followed" "$got" "blue other"
    else note "old: $got"; fi
done
as_version new

echo "-- decoder plugins: the examples show a .jp2 through ffmpeg and a .fits through astropy"
for v in new old; do
    as_version $v
    d=/tmp/decoders-$v
    mkdir -p $d $XDG_DATA_HOME/glimt/decoders
    cp /t/new/examples/decoders/* $XDG_DATA_HOME/glimt/decoders/
    ffmpeg -v error -f lavfi -i color=red:s=600x400 -frames:v 1 -c:v jpeg2000 $d/a.jp2
    # Grey, as telescopes write it: dark at the top, light at the bottom.
    convert -size 600x400 gradient:black-white $d/b.fits
    # Compressed, as large telescope archives keep them: brighter upwards, rows counted from the
    # bottom as FITS does.
    python3 -c "import sys, numpy; from astropy.io import fits
fits.CompImageHDU(numpy.fromfunction(lambda y, x: y, (400, 600), dtype=numpy.float32)).writeto(sys.argv[1])" $d/c.fits.fz
    config
    start $d/a.jp2
    sleep 2
    shot decoder-jp2-$v
    key space; sleep 2
    shot decoder-fits-$v
    key space; sleep 2
    shot decoder-fz-$v
    quit
    if [ $v = new ]; then
        expect "a .jp2 through its decoder" "$(colour decoder-jp2-new 640 400)" red
        top=$(red decoder-fits-new 640 120) bottom=$(red decoder-fits-new 640 700)
        expect "a .fits through its decoder, stretched" "$([ $top -lt 60 ] && [ $bottom -gt 190 ] && echo yes)" yes
        top=$(red decoder-fz-new 760 120) bottom=$(red decoder-fz-new 760 700)
        expect "a compressed .fits.fz, its top at the top" "$([ $top -gt 190 ] && [ $bottom -lt 60 ] && echo yes)" yes
    else note "old: $(colour decoder-jp2-old 640 400), fits $(red decoder-fits-old 640 700)"; fi
done
as_version new

echo "-- science decoders: a NIfTI volume, an HDF5 dataset and a pathology slide shown"
for v in new old; do
    as_version $v
    d=/tmp/science-$v
    mkdir -p $d $XDG_DATA_HOME/glimt/decoders
    cp /t/new/examples/decoders/* $XDG_DATA_HOME/glimt/decoders/
    # Brighter to the front (y) in the volume, brighter downwards in the dataset: the front of a
    # brain slice is shown at the top, a dataset's first row at the top.
    python3 - "$d" <<'PY'
import sys, numpy, nibabel, h5py
d = sys.argv[1]
volume = numpy.fromfunction(lambda x, y, z: y, (40, 50, 30), dtype=numpy.float32)
nibabel.save(nibabel.Nifti1Image(volume, numpy.eye(4)), f"{d}/a.nii.gz")
with h5py.File(f"{d}/b.h5", "w") as f:
    f.create_dataset("data/temperature", data=numpy.fromfunction(lambda r, c: r, (100, 200)))
    # Coordinate bounds as a climate file has them, listed first: not the picture.
    f.create_dataset("data/lat_bnds", data=numpy.full((100, 2), 9.0))
PY
    convert -size 1024x768 xc:red -define tiff:tile-geometry=256x256 -compress jpeg ptif:$d/c.svs
    config
    start $d/a.nii.gz
    sleep 3
    shot science-nifti-$v
    key space; sleep 3
    shot science-hdf5-$v
    key space; sleep 3
    shot science-slide-$v
    quit
    if [ $v = new ]; then
        expect "NIfTI: the front at the top" \
            "$([ $(red science-nifti-new 760 120) -gt 200 ] && [ $(red science-nifti-new 760 700) -lt 60 ] && echo yes)" yes
        expect "HDF5: the first row at the top" \
            "$([ $(red science-hdf5-new 760 200) -lt 60 ] && [ $(red science-hdf5-new 760 620) -gt 200 ] && echo yes)" yes
        expect "a pathology slide through OpenSlide" "$(colour science-slide-new 760 412)" red
    else note "old: $(red science-nifti-old 760 120) $(red science-hdf5-old 760 620) $(colour science-slide-old 760 412)"; fi
done
as_version new

echo "-- a NIfTI series in FSL's orientation (LAS, 4D): its first volume, the front at the top"
d=/tmp/nifti-series
mkdir -p $d $XDG_DATA_HOME/glimt/decoders
cp /t/new/examples/decoders/* $XDG_DATA_HOME/glimt/decoders/
python3 - "$d" <<'PY'
import sys, numpy, nibabel
# Brighter to the front (y); x runs left (LAS); 20 volumes, only the first read.
series = numpy.fromfunction(lambda x, y, z, t: y, (40, 50, 30, 20), dtype=numpy.float32)
nibabel.save(nibabel.Nifti1Image(series, numpy.diag([-1.0, 1, 1, 1])), f"{sys.argv[1]}/a.nii.gz")
PY
config
start $d/a.nii.gz
sleep 3
shot nifti-series
quit
expect "the front at the top" \
    "$([ $(red nifti-series 760 120) -gt 200 ] && [ $(red nifti-series 760 700) -lt 60 ] && echo yes)" yes

echo "-- the Bio-Formats decoder: a microscope file (ICS) shown through bftools"
for v in new old; do
    as_version $v
    d=/tmp/bioformats-$v
    mkdir -p $d $XDG_DATA_HOME/glimt/decoders
    cp /t/new/examples/decoders/* $XDG_DATA_HOME/glimt/decoders/
    # ICS, which the example leaves out for calendar invitations, added as its comment says.
    sed -i 's/^Extensions=czi;/Extensions=ics;ids;czi;/' $XDG_DATA_HOME/glimt/decoders/bioformats.desktop
    # Dark at the top, light at the bottom, 16 bits as a microscope camera writes.
    convert -size 600x400 gradient:black-white -depth 16 /tmp/gradient.tif
    /opt/bftools/bfconvert -no-upgrade -overwrite /tmp/gradient.tif $d/a.ics >/dev/null 2>&1
    config
    export GLIMT_BFTOOLS=/opt/bftools
    start $d/a.ics
    sleep 5
    shot bioformats-$v
    quit
    unset GLIMT_BFTOOLS
    if [ $v = new ]; then
        expect "an ICS file through Bio-Formats" \
            "$([ $(red bioformats-new 760 120) -lt 60 ] && [ $(red bioformats-new 760 700) -gt 190 ] && echo yes)" yes
    else note "old: $(red bioformats-old 760 700)"; fi
done
as_version new

echo "-- the MRC decoder: a cryo-EM map's middle section, its top at the top"
for v in new old; do
    as_version $v
    d=/tmp/mrc-$v
    mkdir -p $d $XDG_DATA_HOME/glimt/decoders
    cp /t/new/examples/decoders/* $XDG_DATA_HOME/glimt/decoders/
    # Brighter upwards (y), as MRC counts rows from the bottom.
    python3 - "$d" <<'PY'
import sys, numpy, mrcfile
volume = numpy.fromfunction(lambda z, y, x: y, (20, 400, 600), dtype=numpy.float32)
mrcfile.new(f"{sys.argv[1]}/a.mrc", data=volume).close()
PY
    config
    start $d/a.mrc
    sleep 3
    shot mrc-$v
    quit
    if [ $v = new ]; then
        expect "an MRC map, its top at the top" \
            "$([ $(red mrc-new 760 120) -gt 190 ] && [ $(red mrc-new 760 700) -lt 60 ] && echo yes)" yes
    else note "old: $(red mrc-old 760 120)"; fi
done
as_version new

echo "-- the DICOM decoder: an X-ray in the window it names, MONOCHROME1 the right way"
for v in new old; do
    as_version $v
    d=/tmp/dicom-$v
    mkdir -p $d $XDG_DATA_HOME/glimt/decoders
    cp /t/new/examples/decoders/* $XDG_DATA_HOME/glimt/decoders/
    # 12 bits brighter downwards, a window over the middle half, and white for zero as some
    # X-ray machines write: shown light at the top, dark at the bottom.
    python3 - "$d" <<'PY'
import sys, numpy, pydicom
from pydicom.dataset import Dataset, FileMetaDataset
from pydicom.uid import ExplicitVRLittleEndian, generate_uid, SecondaryCaptureImageStorage
meta = FileMetaDataset()
meta.MediaStorageSOPClassUID = SecondaryCaptureImageStorage
meta.MediaStorageSOPInstanceUID = generate_uid()
meta.TransferSyntaxUID = ExplicitVRLittleEndian
ds = Dataset()
ds.file_meta = meta
ds.SOPClassUID, ds.SOPInstanceUID = meta.MediaStorageSOPClassUID, meta.MediaStorageSOPInstanceUID
ds.PatientName = "Test^Nobody"
ds.Modality = "DX"
ds.Rows, ds.Columns = 400, 600
ds.SamplesPerPixel, ds.PhotometricInterpretation = 1, "MONOCHROME1"
ds.BitsAllocated, ds.BitsStored, ds.HighBit, ds.PixelRepresentation = 16, 12, 11, 0
ds.WindowCenter, ds.WindowWidth = 2048, 2048
ds.PixelData = numpy.fromfunction(lambda r, c: r * 4095 / 399, (400, 600)).astype(numpy.uint16).tobytes()
ds.is_little_endian, ds.is_implicit_VR = True, False
ds.save_as(f"{sys.argv[1]}/a.dcm", write_like_original=False)
PY
    # The same compressed as JPEG 2000, as dental and hospital equipment often stores it.
    gdcmconv --j2k $d/a.dcm $d/b.dcm
    config
    start $d/a.dcm
    sleep 3
    shot dicom-$v
    key space; sleep 3
    shot dicom-j2k-$v
    quit
    if [ $v = new ]; then
        expect "DICOM: windowed, MONOCHROME1 light at the top" \
            "$([ $(red dicom-new 760 120) -gt 190 ] && [ $(red dicom-new 760 700) -lt 60 ] && echo yes)" yes
        expect "DICOM compressed as JPEG 2000" \
            "$([ $(red dicom-j2k-new 760 120) -gt 190 ] && [ $(red dicom-j2k-new 760 700) -lt 60 ] && echo yes)" yes
    else note "old: $(red dicom-old 760 120)"; fi
done
as_version new

echo "-- the ImageMagick decoders: a Photoshop file and a TGA shown, a PNG named .tga not"
for v in new old; do
    as_version $v
    d=/tmp/graphics-$v
    mkdir -p $d $XDG_DATA_HOME/glimt/decoders
    # Each version's own decoders: the new ones name the format for ImageMagick.
    cp /t/$v/examples/decoders/* $XDG_DATA_HOME/glimt/decoders/
    convert -size 600x400 xc:lime $d/a.psd
    convert -size 600x400 xc:blue $d/b.tga
    # Another format under a decoder's extension, as a file from the internet could be.
    convert -size 600x400 xc:red png:$d/c.tga
    config
    start $d/a.psd
    sleep 3
    shot graphics-psd-$v
    key space; sleep 3
    shot graphics-tga-$v
    key space; sleep 3
    shot graphics-disguised-$v
    quit
    if [ $v = new ]; then
        expect "a Photoshop file shown" "$(colour graphics-psd-new 760 412)" green
        expect "a TGA shown" "$(colour graphics-tga-new 760 412)" blue
        differ "a PNG named .tga not read as PNG" "$(colour graphics-disguised-new 760 412)" red
    else note "old: $(colour graphics-psd-old 760 412) $(colour graphics-tga-old 760 412) $(colour graphics-disguised-old 760 412)"; fi
done
as_version new

echo "-- real files of each format (samples.txt): RAW of eight makes, science and graphics files"
for v in new old; do
    as_version $v
    mkdir -p $XDG_DATA_HOME/glimt/decoders
    cp /t/new/examples/decoders/* $XDG_DATA_HOME/glimt/decoders/
    config
    missing=
    for name in $(grep -v '^#' /t/samples.txt | cut -d' ' -f1); do
        # Not downloaded counts as not shown.
        if [ -f /samples/$name ]; then
            start /samples/$name
            for i in $(seq 120); do grep -q "first image" /tmp/glimt.err && break; sleep 0.5; done
            shot sample-$name-$v
            grep -q "first image" /tmp/glimt.err || missing="$missing $name"
            quit
        else
            missing="$missing $name"
        fi
    done
    if [ $v = new ]; then expect "each one shown" "${missing:- none}" " none"
    else note "old, not shown:${missing:- none}"; fi
done
as_version new

echo "-- a long error under the image: broken into lines inside the image area"
for v in new old; do
    as_version $v
    d=/tmp/long-error-$v
    mkdir -p $d
    # A broken JPEG whose name alone is wider than the image area, with no space to break at.
    name=$(printf 'a_very_long_name_of_a_broken_photograph_%.0s' 1 2 3 4).jpg
    echo "not a picture" >$d/$name
    config
    start $d/$name
    sleep 3
    shot long-error-$v
    quit
    # The image area starts at x=240: no text in its first 10 pixels.
    edge=$(convert /t/out/long-error-$v.png -crop 10x400+241+200 -format "%[fx:int(255*maxima.r)]" info:)
    if [ $v = new ]; then expect "nothing at the image area's edge" "$([ $edge -lt 40 ] && echo none)" none
    else note "old, at the edge: $edge"; fi
done
as_version new

echo "-- search terms combined: and, not (-), or (|) and groups ( )"
for v in new old; do
    as_version $v
    d=/tmp/search-terms-$v
    mkdir -p $d
    for f in a_sunset.jpg b_sunset.png c_beach.jpg d_beach.png; do
        convert -size 300x200 xc:red $d/$f
    done
    config
    start $d
    sleep 2
    # The matches of a search, read back: all of them selected, then asked for.
    found() {
        click 120 284; key ctrl+a; xdotool type "$1"; sleep 2
        click 150 337; key ctrl+a; sleep 1
        $BIN --remote selection | tr '\0' '\n' | xargs -n1 basename 2>/dev/null | tr '\n' ' '
    }
    and_not=$(found 'sunset -re:png$')
    either=$(found 'beach | re:png$')
    grouped=$(found '( sunset | beach ) -( re:png$ )')
    quit
    if [ $v = new ]; then
        expect "sunset and not .png" "$and_not" "a_sunset.jpg "
        expect "beach, or .png" "$either" "b_sunset.png c_beach.jpg d_beach.png "
        expect "a group, and not a group" "$grouped" "a_sunset.jpg c_beach.jpg "
    else note "old: '$and_not' '$either' '$grouped'"; fi
done
as_version new

echo "-- a slideshow from a plain list: its order, relative paths, and the list never written"
for v in new old; do
    as_version $v
    d=/tmp/plain-list-$v
    mkdir -p $d/pics
    convert -size 300x200 xc:red $d/pics/a.jpg
    convert -size 300x200 xc:lime $d/pics/b.jpg
    convert -size 300x200 xc:blue $d/pics/c.jpg
    printf '# shown in this order\npics/c.jpg\n"pics/a.jpg"\n%s/pics/b.jpg\n' "$d" >$d/show.txt
    before=$(md5sum <$d/show.txt)
    config confirm_delete=false
    start $d/show.txt
    sleep 1
    first=$($BIN --remote current | tr '\0' '\n' | xargs -n1 basename 2>/dev/null)
    key space; sleep 1
    second=$($BIN --remote current | tr '\0' '\n' | xargs -n1 basename 2>/dev/null)
    # Delete in a list: out of the list in the window only.
    key Delete; sleep 1
    quit
    left="$(ls $d/pics | tr '\n' ' ')$([ "$(md5sum <$d/show.txt)" = "$before" ] && echo same)"
    if [ $v = new ]; then
        expect "the list's order" "$first $second" "c.jpg a.jpg"
        expect "the images and the list untouched" "$left" "a.jpg b.jpg c.jpg same"
    else note "old: '$first' '$second' '$left'"; fi
done
as_version new

echo "-- the desktop asks for high contrast: Glimt takes its High Contrast theme, unless turned off"
d=/tmp/contrast
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
# The portal's answer for contrast; for the rest nothing, as a desktop without it.
cat >/tmp/stub/gdbus <<'STUB'
#!/bin/sh
case "$*" in
*contrast*) echo "(<uint32 1>,)" ;;
*) exit 1 ;;
esac
STUB
chmod +x /tmp/stub/gdbus
for v in new old; do
    as_version $v
    for follow in true false; do
        config "follow_contrast=$follow"
        start $d
        sleep 3
        shot contrast-$v-$follow
        quit
    done
done
rm /tmp/stub/gdbus
as_version new
# The empty part of the file list: black in High Contrast, grey in the Dark theme.
expect "High Contrast while the desktop asks for it" "$(red contrast-new-true 120 700)" 0
differ "the chosen theme when that is turned off" "$(red contrast-new-false 120 700)" 0
note "old: $(red contrast-old-true 120 700)"

echo "-- the size of menus and lists: a setting, and GNOME's text scaling followed"
d=/tmp/ui-scale
mkdir -p $d
convert -size 3000x2000 xc:red $d/a.jpg
printf '#!/bin/sh\necho 1.5\n' >/tmp/stub/gsettings
chmod +x /tmp/stub/gsettings
for v in new old; do
    as_version $v
    # The file list is 240 points wide: twice as wide at 200 %, 1.5 times with GNOME's 1.5.
    for setting in "ui_scale=200 follow_text_scale=false" "ui_scale=100 follow_text_scale=true" \
        "ui_scale=100 follow_text_scale=false"; do
        config $setting
        start $d
        sleep 3
        shot scale-$v-${setting%% *}-${setting##*=}
        quit
    done
done
rm /tmp/stub/gsettings
as_version new
differ "at 200 %, the list where the image began" "$(colour scale-new-ui_scale=200-false 400 400)" red
differ "GNOME's text scaling followed" "$(colour scale-new-ui_scale=100-true 300 400)" red
expect "not followed when turned off" "$(colour scale-new-ui_scale=100-false 300 400)" red
note "old at 200 %: $(colour scale-old-ui_scale=200-false 400 400)"

echo "-- Back from a folder opened in a collection: the collection again"
for v in new old; do
    as_version $v
    d=/tmp/back-collection-$v
    mkdir -p $d/x $d/y
    convert -size 64x64 xc:red $d/x/a.jpg
    convert -size 64x64 xc:blue $d/y/b.jpg
    printf '#GQview collection\n"%s/x/a.jpg"\n"%s/y/b.jpg"\n' $d $d >$d/show.gqv
    config
    start $d/show.gqv
    sleep 2
    $BIN --remote open $d/y >/dev/null 2>&1; sleep 2
    click 22 35 2                             # Back, in the toolbar
    key End; sleep 1
    got=$($BIN --remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    quit
    if [ $v = new ]; then expect "the collection's last image" "$got" "y/b.jpg"
    else note "old: $got"; fi
done
as_version new

echo "-- a collection put in order with keys: one place earlier, then to the end"
for v in new old; do
    as_version $v
    d=/tmp/reorder-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n"%s/c.jpg"\n' $d $d $d >$d/show.gqv
    config
    start $d/show.gqv
    sleep 1
    key space; key ctrl+shift+Up; sleep 1     # b before a
    key Home; key ctrl+shift+End; sleep 1      # then b, shown first, to the end
    quit
    order=$(grep -o '[abc]\.jpg' $d/show.gqv | tr '\n' ' ')
    if [ $v = new ]; then expect "the collection's new order, saved" "$order" "a.jpg c.jpg b.jpg "
    else note "old: $order"; fi
done
as_version new

echo "-- a collection put in order by dragging: a, the first row, dropped below c"
for v in new old; do
    as_version $v
    d=/tmp/drag-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n"%s/c.jpg"\n' $d $d $d >$d/show.gqv
    config
    start $d/show.gqv
    sleep 2
    # Rows at 337, 419 and 501; the lower half of c's row is after it.
    for y in 345 340 337; do xdotool mousemove 100 $y; sleep 0.2; done
    xdotool mousedown 1 2>/dev/null
    for y in $(seq 345 10 525); do xdotool mousemove 100 $y; sleep 0.05; done
    sleep 0.5
    shot drag-line-$v
    xdotool mouseup 1 2>/dev/null
    sleep 1
    quit
    order=$(grep -o '[abc]\.jpg' $d/show.gqv | tr '\n' ' ')
    if [ $v = new ]; then expect "the collection's new order, saved" "$order" "b.jpg c.jpg a.jpg "
    else note "old: $order"; fi
done
as_version new

echo "-- a rectangle dragged while the folder changes: the selection keeps its files"
for v in new old; do
    as_version $v
    d=/tmp/band-refresh-$v
    mkdir -p $d
    for f in b c d e; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d
    sleep 2
    key ctrl+i; sleep 1
    click 19 313                    # b shown; Ctrl+click on c selects both
    xdotool keydown ctrl; click 131 313; xdotool keyup ctrl
    # Ctrl and a rectangle from below the cells up into the second row, which adds d to b and
    # c; a.png comes meanwhile, before them in the list, and ends the rectangle.
    xdotool keydown ctrl; xdotool mousemove 60 650; sleep 0.3
    xdotool mousedown 1; for y in 640 600 520 460; do xdotool mousemove 70 $y; sleep 0.1; done
    sleep 0.5; convert -size 64x64 xc:blue $d/a.png; sleep 3
    xdotool mousemove 75 455; sleep 0.3; xdotool mouseup 1; xdotool keyup ctrl; sleep 1
    got=$($BIN --remote selection 2>/dev/null | tr '\0' ' ' | sed "s|$d/||g")
    quit
    if [ $v = new ]; then expect "b, c and d still selected, not a" "$got" "b.png c.png d.png "
    else note "old: $got"; fi
done
as_version new

echo "-- a rectangle held at the grid's bottom edge: the grid scrolls, the rows passed stay selected"
for v in new old; do
    as_version $v
    d=/tmp/band-scroll-$v
    mkdir -p $d
    convert -size 64x64 xc:red $d/00.png
    for i in $(seq -w 1 59); do cp $d/00.png $d/$i.png; done
    config
    start $d
    sleep 2
    key ctrl+i; sleep 2
    xdotool mousemove 60 320; sleep 0.3; xdotool mousedown 1
    for y in $(seq 330 20 750); do xdotool mousemove 60 $y; sleep 0.05; done
    # Held near the edge: the grid scrolls on, a little movement keeps frames coming.
    for i in $(seq 30); do xdotool mousemove 61 760; sleep 0.1; xdotool mousemove 60 760; sleep 0.1; done
    xdotool mouseup 1; sleep 1
    n=$($BIN --remote selection 2>/dev/null | tr -cd '\0' | wc -c)
    quit
    if [ $v = new ]; then expect "more than the rows in view selected" "$([ $n -ge 20 ] && echo yes)" yes; fi
    note "$v: $n selected"
done
as_version new

echo "-- an empty collection: the keys that move images in it leave Glimt running"
for v in new old; do
    as_version $v
    d=/tmp/empty-collection-$v
    mkdir -p $d
    printf '#GQview collection\n' >$d/empty.gqv
    config
    start $d/empty.gqv
    sleep 2
    key ctrl+shift+Up; key ctrl+shift+End; sleep 1
    running=$(kill -0 $GPID 2>/dev/null && echo running || echo crashed)
    quit
    if [ $v = new ]; then expect "Glimt still runs" "$running" running
    else note "old: $running"; fi
done
as_version new

echo "-- a collection played in random order: an image taken out of it keeps the others' order"
for v in new old; do
    as_version $v
    d=/tmp/shuffled-$v
    mkdir -p $d
    for c in a b c d e f g h; do convert -size 300x200 xc:red $d/$c.jpg; done
    printf '#GQview collection\n' >$d/show.gqv
    for c in a b c d e f g h; do printf '"%s/%s.jpg"\n' $d $c >>$d/show.gqv; done
    config random=true
    start $d/show.gqv
    sleep 2
    key s; sleep 1          # the slideshow, shuffled
    key Delete; sleep 1     # the shown one out of the collection while shuffled
    key s; sleep 1          # stopped: back in the collection's order
    quit
    order=$(grep -o '[a-h]\.jpg' $d/show.gqv | tr '\n' ' ')
    # Seven left, in the order a to h.
    kept=$([ "$order" = "$(echo $order | tr ' ' '\n' | sort | tr '\n' ' ')" ] && echo $order | wc -w)
    if [ $v = new ]; then expect "saved in its own order" "$kept" 7
    else note "old: $order"; fi
done
as_version new

echo "-- Check Images: a photo cut off in copying and a file that is no picture found and marked"
for v in new old; do
    as_version $v
    d=/tmp/check-images-$v
    mkdir -p $d
    convert -size 600x400 gradient:red-blue $d/a_whole.jpg
    head -c $(( $(stat -c %s $d/a_whole.jpg) * 2 / 3 )) $d/a_whole.jpg >$d/b_cut.jpg
    echo "not a picture" >$d/c_text.png
    config
    start $d
    sleep 2
    # File → Check Images…, Check, and the window closed when done to see the list.
    click 20 10; sleep 1
    click 66 540; sleep 1
    click 56 127; sleep 4
    shot check-window-$v
    click 554 38; sleep 1
    shot check-list-$v
    quit
    # The warning sign at the right end of a row of the list: orange (red 240), or the row's own
    # dark grey or selection blue.
    marked() { [ $(red check-list-$v 214 $1) -gt 200 ] && echo marked || echo -; }
    got="$(marked 337) $(marked 419) $(marked 501)"
    if [ $v = new ]; then expect "the cut and the false one marked, the whole one not" "$got" "- marked marked"
    else note "old: $got"; fi
    # The same marks in the grid of thumbnails (Ctrl+I), on each thumbnail's top left.
    if [ $v = new ]; then
        start $d; sleep 2
        click 20 10; sleep 1; click 66 540; sleep 1; click 56 127; sleep 4; click 554 38; sleep 1
        key ctrl+i; sleep 2
        shot check-grid-$v
        quit
        # Cells of 112: a_whole and b_cut in the first row, c_text below.
        # Orange (240 140 0), told from a red thumbnail by its green.
        cell() {
            set -- $(pixel check-grid-$v $1 $2)
            [ $1 -gt 200 ] && [ $2 -gt 100 ] && [ $2 -lt 180 ] && echo marked || echo -
        }
        expect "the same marks in the grid" "$(cell 19 313) $(cell 131 313) $(cell 19 417)" "- marked marked"
    fi
done
as_version new

echo "-- Check Images still running when another folder opens: nothing marked there"
for v in new old; do
    as_version $v
    d=/tmp/check-switch-$v
    mkdir -p $d/a $d/b
    convert -size 600x400 gradient:red-blue $d/whole.jpg
    head -c $(( $(stat -c %s $d/whole.jpg) * 2 / 3 )) $d/whole.jpg >$d/cut.jpg
    # Folder a: 20,000 cut photos, checked for some seconds; b: whole ones named as a's last.
    for i in $(seq -w 1 20000); do ln $d/cut.jpg $d/a/n_$i.jpg; done
    for i in 19998 19999 20000; do cp $d/whole.jpg $d/b/n_$i.jpg; done
    config
    start $d/a
    sleep 2
    click 20 10; sleep 1
    click 66 540; sleep 1
    click 56 127; sleep 2
    # The check runs: a's first files, cut, are marked before b opens.
    shot check-switch-a-$v
    $BIN --remote open $d/b >/dev/null 2>&1
    sleep 15
    shot check-switch-$v
    quit
    marked() { [ $(red $1 214 $2) -gt 200 ] && echo marked || echo -; }
    before="$(marked check-switch-a-$v 501) $(marked check-switch-a-$v 583)"   # below its window
    got="$(marked check-switch-$v 337) $(marked check-switch-$v 419) $(marked check-switch-$v 501)"
    if [ $v = new ]; then
        expect "Check Images marks the cut files of the folder it began in" "$before" "marked marked"
        expect "the new folder's files not marked" "$got" "- - -"
    else note "old: $before, then $got"; fi
done
as_version new

echo "-- a settings file with a byte that is not UTF-8: a menu toggle leaves the rest as it was"
d=/tmp/config-latin1
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
for v in new old; do
    as_version $v
    printf 'delay=7\nvideo_player=/opt/caf\351/player\n' >$XDG_CONFIG_HOME/glimt/config
    start $d/a.jpg
    sleep 2
    key t; key t          # Thumbnails off and on: each saves the settings
    sleep 1
    quit
    got=$(grep -c '^delay=7$' $XDG_CONFIG_HOME/glimt/config)
    if [ $v = new ]; then expect "the own delay kept" "$got" 1
    else note "old: delay=7 lines: $got"; fi
done
as_version new

echo "-- Select by mark: the files with mark 1 selected"
d=/tmp/select-mark
mkdir -p $d
for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
config
start $d/a.png
sleep 2
key space; key 1; key space; key 1          # mark 1 on b and c
key ctrl+1; sleep 1                          # Select by Mark 1
got=$($BIN --remote selection 2>/dev/null | tr '\0' ' ' | sed "s|$d/||g")
quit
expect "b and c selected" "$got" "b.png c.png "

echo "-- Back Up Glimt's Data: settings, marks and collections copied to a dated folder"
for v in new old; do
    as_version $v
    d=/tmp/backup-$v
    mkdir -p $d/to $XDG_DATA_HOME/glimt/collections $XDG_STATE_HOME/glimt
    convert -size 300x200 xc:red $d/a.jpg
    printf '#GQview collection\n"%s/a.jpg"\n' $d >$XDG_DATA_HOME/glimt/collections/Mine.gqv
    printf '0\t4\t%s/a.jpg\n' $d >$XDG_STATE_HOME/glimt/marks
    config theme=Light
    answer $d/to
    start $d
    sleep 2
    click 20 10; sleep 1
    click 80 564; sleep 3     # File → Back Up Glimt's Data…
    quit
    b=$(ls -d $d/to/glimt-backup-* 2>/dev/null | head -1)
    got="$(cat "$b/config/config" 2>/dev/null | grep -c theme=Light) $(ls "$b/data/collections" 2>/dev/null) $(cut -f2 "$b/state/marks" 2>/dev/null)"
    if [ $v = new ]; then expect "the settings, the collection and the stars in the backup" "$got" "1 Mine.gqv 4"
    else note "old: $got"; fi
done
as_version new

echo "-- glimt --back-up FOLDER: the same backup without a window, a second one beside the first"
for v in new old; do
    as_version $v
    d=/tmp/backup-cli-$v
    mkdir -p $d/to $XDG_DATA_HOME/glimt/collections
    printf '#GQview collection\n"%s/a.jpg"\n' $d >$XDG_DATA_HOME/glimt/collections/Mine.gqv
    config theme=Light
    $BIN --back-up $d/to >$d/out 2>&1; status=$?
    $BIN --back-up $d/to >/dev/null 2>&1
    got="$status $(ls $d/to | wc -l) $(ls $(ls -d $d/to/glimt-backup-* 2>/dev/null | head -1)/data/collections 2>/dev/null)"
    if [ $v = new ]; then expect "two backups, the collection in them" "$got" "0 2 Mine.gqv"
    else note "old: $got $(head -c 60 $d/out)"; fi
done
as_version new

echo "-- glimt --back-up: a folder that is not there is refused, a running rename's plan left out"
for v in new old; do
    as_version $v
    d=/tmp/backup-checks-$v
    mkdir -p $d/to $XDG_DATA_HOME/glimt/collections $XDG_STATE_HOME/glimt/renames
    printf '#GQview collection\n' >$XDG_DATA_HOME/glimt/collections/Mine.gqv
    printf '/x\0/y\0' >$XDG_STATE_HOME/glimt/renames/plan
    $BIN --back-up $d/typo >/dev/null 2>&1; missing=$?
    $BIN --back-up $XDG_DATA_HOME/glimt >/dev/null 2>&1; inside=$?
    [ -d $XDG_DATA_HOME/glimt/glimt-backup-* ] && inside="$inside, made"
    $BIN --back-up $d/to >/dev/null 2>&1
    got="$missing $(ls -d $d/typo 2>/dev/null | wc -l) $(ls $d/to/glimt-backup-*/state 2>/dev/null | grep -c renames)"
    # Gone again: every later start would find it and ask about a rename cut short.
    rm -rf $XDG_STATE_HOME/glimt/renames
    if [ $v = new ]; then expect "refused, nothing made; no plan in the backup" "$got" "1 0 0"
        expect "a backup into Glimt's own folder refused" "$inside" 1
    else note "old: $got"; fi
done
as_version new

echo "-- Faster without a lock: the delay saved (so the two checks below test something)"
d=/tmp/faster-saved
mkdir -p $d
for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
config delay=5
start $d
sleep 2
key ctrl+equal; sleep 1
quit
expect "the delay changed" "$(grep -c 'delay=5$' $XDG_CONFIG_HOME/glimt/config)" 0

echo "-- a kiosk: a visitor's keys change no saved setting"
for v in new old; do
    as_version $v
    d=/tmp/kiosk-keys-$v
    mkdir -p $d
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    config delay=5
    start --kiosk $d
    sleep 2
    key ctrl+equal; key r; key i; sleep 1    # faster, random order, overlay
    $BIN --remote-all quit >/dev/null 2>&1; sleep 1
    quit
    got=$(grep -c 'delay=5$' $XDG_CONFIG_HOME/glimt/config)
    if [ $v = new ]; then expect "the delay as it was" "$got" 1
    else note "old: $(grep -E '^(delay|random|overlay)=' $XDG_CONFIG_HOME/glimt/config | tr '\n' ' ')"; fi
done
as_version new

echo "-- --lock-settings, then New Window: the new window is locked too"
for v in new old; do
    as_version $v
    d=/tmp/lock-new-window-$v
    mkdir -p $d
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    config delay=5
    start --lock-settings $d
    sleep 2
    key ctrl+n; sleep 3                   # a new window: Faster pressed there
    other=$(xdotool search --name Glimt 2>/dev/null | grep -vx "$WIN" | tail -1)
    [ $v = new ] && expect "a second window opened" "$([ -n "$other" ] && echo yes)" yes
    xdotool windowfocus $other key ctrl+equal 2>/dev/null; sleep 1
    $BIN --remote-all quit >/dev/null 2>&1; sleep 1
    quit
    got=$(grep -c 'delay=5$' $XDG_CONFIG_HOME/glimt/config)
    if [ $v = new ]; then expect "the delay as it was" "$got" 1
    else note "old: $(grep '^delay=' $XDG_CONFIG_HOME/glimt/config)"; fi
done
as_version new

echo "-- the keyboard focus shown: Tab to a toolbar button, a ring in the accent colour"
d=/tmp/focus
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
for v in new old; do
    as_version $v
    config
    start $d
    sleep 2
    key Tab; key Tab; sleep 1
    shot focus-tab-$v
    click 99 36; sleep 1    # a click on a button: the ring goes
    shot focus-click-$v
    quit
done
as_version new
# The left edge of the ring around Go, the second Tab stop: the accent (blue, 53 132 228) after
# Tab, the menu bar's dark grey after a click.
ring() {
    for x in 40 41; do
        set -- $1 $(pixel $1 $x 11)
        [ $2 -lt 100 ] && [ $4 -gt 180 ] && { echo ring; return; }
        set -- $1
    done
    echo -
}
expect "Tab shows where the focus is" "$(ring focus-tab-new)" ring
expect "a click takes the ring away" "$(ring focus-click-new)" -
note "old: $(ring focus-tab-old)"

echo "-- view filters: Shift+G shows a red image grey, then inverted, and the filter is kept"
d=/tmp/filters
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
for v in new old; do
    as_version $v
    config
    start $d
    sleep 2
    key shift+g; sleep 2
    shot filter-grey-$v
    key shift+g; sleep 2
    shot filter-invert-$v
    quit
    [ $v = new ] && kept=$(grep -c '^view_filter=2$' $XDG_CONFIG_HOME/glimt/config)
done
as_version new
# Red through Rec. 709 greyscale is 53 53 53; inverted it is cyan.
filtered() {
    set -- $(pixel $1 760 412)
    if [ $1 -gt 40 ] && [ $1 -lt 70 ] && [ $1 = $2 ] && [ $2 = $3 ]; then echo grey
    elif [ $1 -lt 30 ] && [ $2 -gt 220 ] && [ $3 -gt 220 ]; then echo cyan
    else echo "$*"; fi
}
expect "greyscale" "$(filtered filter-grey-new)" grey
expect "inverted" "$(filtered filter-invert-new)" cyan
expect "the filter kept in the settings" "$kept" 1
note "old: $(filtered filter-grey-old)"

echo "-- Tab to a menu, then a click on that same menu: the shortcuts work again"
for v in new old; do
    as_version $v
    d=/tmp/focus-click-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d
    sleep 2
    key Tab; key Tab; sleep 0.5         # Go, the second Tab stop
    click 52 11; click 52 11            # Go opened and closed by clicks
    key End; sleep 1
    got=$($BIN --remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    quit
    if [ $v = new ]; then expect "End shows the last image" "$got" "c.png"
    else note "old: $got"; fi
done
as_version new

echo "-- the projector shows the frame of an animation stepped to"
for v in new old; do
    as_version $v
    d=/tmp/projector-frame-$v
    mkdir -p $d
    # Red for 20 s, then blue: stepped to, the blue frame is shown at once.
    convert -delay 2000 -size 300x200 xc:red xc:blue $d/a.gif
    config
    start $d/a.gif
    sleep 2
    click 225 10; click 236 95 2       # Window → Projector
    key ctrl+Next; sleep 1             # the next frame: blue
    p=$(xdotool search --name "Glimt Projector" 2>/dev/null | head -1)
    [ -n "$p" ] && import -window $p /t/out/projector-frame-$v.png
    quit
    if [ $v = new ]; then expect "the blue frame on the projector" "$(colour projector-frame-new 480 300)" blue
    else note "old: $(colour projector-frame-old 480 300 2>/dev/null)"; fi
done
as_version new

echo "-- a projector larger than the main view: the image sharp there"
d=/tmp/projector-sharp
mkdir -p $d
# Stripes 4 pixels wide: kept at the projector's scale, grey at the main view's.
convert -size 3200x2000 xc: -fx 'i%8<4' $d/a.png
convert $d/a.png -resize 50% $d/half.png
for v in new old; do
    as_version $v
    config
    start $d/a.png
    sleep 2
    click 225 10; click 236 95 2       # Window → Projector
    p=$(xdotool search --name "Glimt Projector" 2>/dev/null | head -1)
    # As full screen on a large screen; Xvfb has no window manager to make it so.
    xdotool windowsize $p 1600 1000 2>/dev/null; sleep 4
    rm -f /t/out/projector-sharp-$v.png
    [ -n "$p" ] && import -window $p /t/out/projector-sharp-$v.png
    quit
    # How far from the picture at the projector's scale (0.5 exactly): RMSE in tenths of a
    # percent; decoded for the main view and stretched, it is blurred and off by more. No
    # number (no projector, no shot) leaves it empty, which fails.
    eval "off_$v=$(compare -metric RMSE /t/out/projector-sharp-$v.png $d/half.png null: 2>&1 \
        | grep -o '([0-9.e+-]*)' | tr -d '()' | awk '{print int($1 * 1000)}')"
done
as_version new
expect "the stripes as they are on the projector" "$([ ${off_new:-999} -le 50 ] && echo yes)" yes
note "off by (tenths of a percent): new $off_new, old $off_old"

echo "-- the keyword tree: a box sets the keyword, with its place in the tree"
d=/tmp/keywords
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
printf 'Animals\nPlaces|Norway|Oslo\n' >$XDG_CONFIG_HOME/glimt/keywords
config exif_panel=true
start $d
sleep 2
# Frames drawn as the pointer moves: a panel that grows with each moves the boxes away.
# The tree's boxes are below Keywords, Comment, Alt text and Extended description.
for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
sleep 1
shot keywords-before
# The Animals box: on, then off again.
click 1056 420 2
shot keywords-on
on=$(cat $d/a.jpg.xmp 2>/dev/null | grep -c '<rdf:li>Animals</rdf:li>')
click 1056 420 2
off=$(cat $d/a.jpg.xmp 2>/dev/null | grep -c '<rdf:li>Animals</rdf:li>')
quit
expect "a box sets the keyword and its tree path" "$on" 2
expect "a box clears them again" "$off" 0

echo "-- the keyword tree: removing an inner level takes the levels below it"
d=/tmp/keywords-remove
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
for v in new old; do
    as_version $v
    printf 'Animals\nPlaces|Norway|Oslo\n' >$XDG_CONFIG_HOME/glimt/keywords
    config exif_panel=true
    start $d
    sleep 2
    for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
    sleep 1
    # Right-click Places, which holds only Norway|Oslo, and Remove from the tree in its menu.
    xdotool mousemove 1080 444 click 3 2>/dev/null
    sleep 1
    shot keywords-remove-menu-$v
    click 1130 460 2
    shot keywords-remove-$v
    quit
    left=$(tr '\n' ' ' <$XDG_CONFIG_HOME/glimt/keywords | sed 's/ $//')
    if [ $v = new ]; then expect "removing Places takes Norway and Oslo with it" "$left" Animals
    else note "old, left in the tree: $left"; fi
done
as_version new

echo "-- a Lightroom catalog: its collection, keyword tree, stars and image keywords imported"
d=/tmp/lr
mkdir -p $d/photos/2024
convert -size 300x200 xc:red $d/photos/2024/a.jpg
convert -size 300x200 xc:blue $d/photos/2024/b.jpg
# The tables and columns Glimt reads, as Lightroom Classic has them.
sqlite3 $d/Catalog.lrcat <<SQL
CREATE TABLE AgLibraryRootFolder (id_local INTEGER PRIMARY KEY, absolutePath);
CREATE TABLE AgLibraryFolder (id_local INTEGER PRIMARY KEY, rootFolder, pathFromRoot);
CREATE TABLE AgLibraryFile (id_local INTEGER PRIMARY KEY, folder, baseName, extension);
CREATE TABLE Adobe_images (id_local INTEGER PRIMARY KEY, rootFile, rating);
CREATE TABLE AgLibraryCollection (id_local INTEGER PRIMARY KEY, creationId, name, parent);
CREATE TABLE AgLibraryCollectionImage (id_local INTEGER PRIMARY KEY, collection, image);
CREATE TABLE AgLibraryKeyword (id_local INTEGER PRIMARY KEY, name, parent);
CREATE TABLE AgLibraryKeywordImage (id_local INTEGER PRIMARY KEY, image, tag);
INSERT INTO AgLibraryRootFolder VALUES (1, '$d/photos/');
INSERT INTO AgLibraryFolder VALUES (2, 1, '2024/');
INSERT INTO AgLibraryFile VALUES (3, 2, 'a', 'jpg'), (4, 2, 'b', 'jpg');
INSERT INTO Adobe_images VALUES (5, 3, 4.0), (6, 4, 3);
INSERT INTO AgLibraryCollection VALUES (7, 'com.adobe.ag.library.group', 'Travel', NULL),
    (8, 'com.adobe.ag.library.collection', 'Norway', 7);
INSERT INTO AgLibraryCollectionImage VALUES (9, 8, 5), (10, 8, 6);
INSERT INTO AgLibraryKeyword VALUES (11, NULL, NULL), (12, 'Places', 11), (13, 'Oslo', 12);
INSERT INTO AgLibraryKeywordImage VALUES (14, 6, 13);
SQL
# There already: the collection with one of the images, and other stars on that image.
mkdir -p $XDG_DATA_HOME/glimt/collections $XDG_STATE_HOME/glimt
printf '#GQview collection\n"%s"\n' $d/photos/2024/a.jpg \
    >"$XDG_DATA_HOME/glimt/collections/Travel - Norway.gqv"
printf '0\t2\t%s\n' $d/photos/2024/a.jpg >$XDG_STATE_HOME/glimt/marks
config xmp_stars=true
start $d/photos/2024
sleep 2
answer $d/Catalog.lrcat
click 20 11
click 100 444 3   # File → Import Lightroom Catalog…
shot lightroom-dialog
click 382 447 2   # Next
shot lightroom-plan
click 421 474 3   # Import, keeping Glimt's stars, with the images' keywords
shot lightroom-done
quit
coll="$XDG_DATA_HOME/glimt/collections/Travel - Norway.gqv"
expect "the collection that was there gets the image it lacked, the other once" \
    "$(grep -c "^\"$d/photos/2024/[ab].jpg\"$" "$coll" 2>/dev/null)" 2
expect "the keyword under its parent" "$(grep -cx 'Places|Oslo' $XDG_CONFIG_HOME/glimt/keywords)" 1
stars=$(sort $XDG_STATE_HOME/glimt/marks | cut -f2 | tr '\n' ' ')
expect "Glimt's stars kept, the new ones taken" "$stars" "2 3 "
expect "the new stars in XMP too" "$(grep -c 'xmp:Rating="3"' $d/photos/2024/b.jpg.xmp)" 1
expect "the image's keyword in its sidecar, with its place in the tree" \
    "$(grep -c '<rdf:li>Oslo</rdf:li>\|<rdf:li>Places|Oslo</rdf:li>' $d/photos/2024/b.jpg.xmp)" 2

echo "-- a plain list (.m3u) is never renamed or trashed, also by keys of one's own"
for v in new old; do
    as_version $v
    # In the home folder, where gio has a trash.
    d=$HOME/list-kept-$v
    mkdir -p $d
    convert -size 300x200 xc:red $d/a.jpg
    printf '#EXTM3U\na.jpg\n' >$d/show.m3u
    config
    start $d/show.m3u
    sleep 2
    # File → Move Collection to Trash where 0.30.0 has it; without it, Find Duplicates is there.
    click 20 11
    click 93 564 2
    quit
    got="$(cat $d/show.m3u 2>/dev/null | tr '\n' ' ')"
    if [ $v = new ]; then expect "the list as it was" "$got" "#EXTM3U a.jpg "
    else note "old: '$got'"; fi
done
as_version new

echo "-- an image of a collection moved to another folder: the collection follows it"
for v in new old; do
    as_version $v
    d=/tmp/collection-move-$v
    mkdir -p $d/to
    for f in a b; do convert -size 64x64 xc:red $d/$f.jpg; done
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n' $d $d >$d/show.gqv
    config
    start $d/show.gqv
    sleep 2
    answer $d/to
    key ctrl+m; sleep 2                  # a, shown, to the folder "to"
    quit
    got="$(grep -o '[a-z/-]*/[ab]\.jpg' $d/show.gqv | sed "s|$d/||" | tr '\n' ' ') $(ls $d/to)"
    if [ $v = new ]; then expect "the collection names a where it went" "$got" "to/a.jpg b.jpg  a.jpg"
    else note "old: $got"; fi
done
as_version new

echo "-- a collection renamed by another program while shown: not made again under its old name"
for v in new old; do
    as_version $v
    d=/tmp/coll-gone-$v
    mkdir -p $d
    for f in a b; do convert -size 64x64 xc:red $d/$f.jpg; done
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n' $d $d >$d/Trip.gqv
    config
    start $d/Trip.gqv
    sleep 2
    mv $d/Trip.gqv $d/Holiday.gqv            # renamed elsewhere meanwhile
    key Delete; sleep 1                      # the shown one out of the collection: a save
    quit
    got="$(cd $d && ls *.gqv | tr '\n' ' ')"
    if [ $v = new ]; then expect "only Holiday.gqv" "$got" "Holiday.gqv "
    else note "old: $got"; fi
done
as_version new

echo "-- a collection renamed, then moved to the trash: its images stay"
d=/tmp/coll
mkdir -p $d $XDG_DATA_HOME/glimt/collections
convert -size 300x200 xc:red $d/a.jpg
c=$XDG_DATA_HOME/glimt/collections
# Without the collections of the sections before.
rm -rf $c && mkdir -p $c
printf '#GQview collection\n"%s"\n' $d/a.jpg >$c/Old.gqv
rm -rf $H/.local/share/Trash
config
start $c/Old.gqv
sleep 2
click 20 11
click 80 540   # File → Rename Collection…
key ctrl+a; xdotool type New; key Return
renamed=$(ls $c | tr '\n' ' ')
click 20 11
click 93 564 2   # File → Move Collection to Trash
shot collection-trashed
quit
expect "renamed in its folder" "$renamed" "New.gqv "
expect "the collection in the trash" "$(ls $c; ls $H/.local/share/Trash/files)" "New.gqv"
expect "its image stays" "$(ls $d)" "a.jpg"
echo "-- / puts the cursor in the search field, without typing itself there"
d=/tmp/slash
mkdir -p $d
convert -size 300x200 xc:red $d/apple_red.jpg
convert -size 300x200 xc:blue $d/sky_blue.jpg
for v in new old; do
    as_version $v
    config
    start $d/apple_red.jpg
    sleep 2
    key slash; xdotool type sky; sleep 2
    shot slash-$v
    quit
    # The first row of the list: the blue picture when the search found it.
    if [ $v = new ]; then expect "the search shows only sky_blue.jpg" "$(colour slash-new 57 337)" blue
    else note "old, first row: $(colour slash-old 57 337)"; fi
done
as_version new
# From full screen too: / brings the list back and the cursor into its search.
config
start $d/apple_red.jpg
sleep 2
key f; sleep 2
key slash; sleep 1; xdotool type sky; sleep 2
shot slash-full
quit
expect "from full screen, the search shows sky_blue.jpg" "$(colour slash-full 57 337)" blue

echo "-- a folder named like an image, a pipe and a link to /dev/zero: listed as they are"
# Only the new version: the old one reads /dev/zero until memory runs out.
d=/tmp/kinds
mkdir -p $d/2019.jpg
convert -size 300x200 xc:red $d/a.jpg
ln -s /dev/zero $d/zero.jpg
mkfifo $d/pipe.jpg
config
start $d
sleep 4
shot kinds
alive=$(kill -0 $GPID 2>/dev/null && echo yes || echo no)
mb=$(awk '/RssAnon/ {print int($2 / 1024)}' /proc/$GPID/status 2>/dev/null)
quit
expect "Glimt still runs" "$alive" yes
if [ "${mb:-9999}" -lt 400 ]; then pass "memory stays small: $mb MB"; else fail "memory: ${mb:-?} MB"; fi

echo "-- printing to a PDF named as one of the images printed: that image is read first"
d=/tmp/printself
for v in new old; do
    as_version $v
    rm -rf $d && mkdir -p $d
    convert -size 300x200 xc:red $d/a.jpg
    convert -size 300x200 xc:blue $d/b.jpg
    config
    start $d/a.jpg
    sleep 2
    answer $d/a.jpg; key ctrl+a; key ctrl+p; sleep 1; key Return
    for i in $(seq 30); do pdfinfo $d/a.jpg >/dev/null 2>&1 && break; sleep 1; done
    sleep 1
    quit
    pages=$(pdfinfo $d/a.jpg 2>/dev/null | awk '/^Pages:/ {print $2}')
    if [ $v = new ]; then expect "both images in the PDF" "$pages" 2
    else note "old, pages: ${pages:-none}"; fi
done
as_version new

echo "-- printing to a PDF when no image can be read: the file chosen is left as it was"
d=/tmp/print-none
for v in new old; do
    as_version $v
    rm -rf $d && mkdir -p $d
    echo "not a picture" >$d/a.jpg
    echo "keep" >$d/out.pdf
    config
    start $d/a.jpg
    sleep 2
    answer $d/out.pdf; key ctrl+p; sleep 1; key Return; sleep 4
    quit
    got=$(cat $d/out.pdf | head -c 4)
    if [ $v = new ]; then expect "out.pdf untouched" "$got" "keep"
    else note "old: $got"; fi
done
as_version new

echo "-- browsing past images writes nothing: a keyword with a comma, a comment with a newline"
d=/tmp/browse
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
convert -size 300x200 xc:blue $d/b.jpg
convert -size 300x200 xc:lime $d/c.jpg
cat >$d/a.jpg.xmp <<XMP
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:subject><rdf:Bag><rdf:li>Smith, John</rdf:li></rdf:Bag></dc:subject>
  <dc:description><rdf:Alt><rdf:li xml:lang="x-default">Two lines
</rdf:li></rdf:Alt></dc:description>
 </rdf:Description></rdf:RDF></x:xmpmeta>
XMP
cp $d/a.jpg.xmp /tmp/browse-a.xmp
exiftool -q -overwrite_original "-XMP-dc:Subject=Smith, John" $d/b.jpg
for v in new old; do
    as_version $v
    cp /tmp/browse-a.xmp $d/a.jpg.xmp
    rm -f $d/b.jpg.xmp
    config exif_panel=true
    start $d/a.jpg
    sleep 2
    key space; sleep 2; key space; sleep 2
    quit
    got="$(cmp -s $d/a.jpg.xmp /tmp/browse-a.xmp && echo same || echo changed) $(ls $d/b.jpg.xmp 2>/dev/null | wc -l)"
    if [ $v = new ]; then expect "the sidecar kept, no new one" "$got" "same 0"
    else note "old: $got"; fi
done
as_version new

echo "-- an exif: search after a keyword was set finds it, without indexing all again"
d=/tmp/exifsearch
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
convert -size 300x200 xc:blue $d/b.jpg
printf 'Zebra\n' >$XDG_CONFIG_HOME/glimt/keywords
config exif_panel=true
start $d/b.jpg
sleep 2
search() { click 120 284; key ctrl+a; xdotool type "$1"; sleep 3; }
search exif:zebra
shot exif-search-0
before=$(colour exif-search-0 57 337)
key ctrl+a; key BackSpace; key Escape; sleep 1
click 1056 420 2   # the Zebra box, for b.jpg
search exif:zebra
shot exif-search-1
quit
expect "nothing before, b.jpg after" "$before $(colour exif-search-1 57 337)" "other blue"

echo "-- Rename Many cut short by a crash: the old names put back at the next start"
d=/tmp/cutshort
mkdir -p $d $XDG_STATE_HOME/glimt/renames
convert -size 300x200 xc:red jpg:$d/.glimt-rename-x-0
convert -size 300x200 xc:blue jpg:$d/.glimt-rename-x-1
# The plan of a swap of a.jpg and b.jpg, stopped with both under their temporary names.
printf '%s\0' $d/a.jpg $d/.glimt-rename-x-0 $d/b.jpg $d/.glimt-rename-x-1 \
    $d/.glimt-rename-x-0 $d/b.jpg $d/.glimt-rename-x-1 $d/a.jpg >$XDG_STATE_HOME/glimt/renames/crashed
config
start $d
sleep 3
click 444 425 3   # Put the old names back
shot cut-short
quit
# a.jpg was the red one: more than half red, as JPEG rounds.
r=$(convert $d/a.jpg -format "%[fx:p{150,100}.r>0.5]" info: 2>/dev/null)
got="$(ls -A $d | tr '\n' ' ')$r $(ls -A $XDG_STATE_HOME/glimt/renames | wc -l)"
expect "a.jpg (red) and b.jpg back, the plan gone" "$got" "a.jpg b.jpg 1 0"
# A plan left by a failure here would be put back at every later start.
rm -rf $XDG_STATE_HOME/glimt/renames

echo "-- size and date columns with thumbnails: the list wide enough for the names"
d=/tmp/columns
mkdir -p $d
convert -size 300x200 xc:red $d/a_rather_long_name_of_a_red_picture.jpg
for v in new old; do
    as_version $v
    config file_details=true
    start $d/a_rather_long_name_of_a_red_picture.jpg
    sleep 2
    shot columns-$v
    quit
    # Thumbnail, name and both columns: the list ends past x=465, where the red image was.
    if [ $v = new ]; then differ "the list has room for the name" "$(colour columns-new 465 400)" red
    else note "old, at x=465: $(colour columns-old 465 400)"; fi
done
as_version new

echo "-- thumbnails without columns: the list is never dragged narrower than a thumbnail"
d=/tmp/narrow
mkdir -p $d
# As wide as the window: red from one edge of the view to the other.
convert -size 6000x800 xc:red $d/wide.jpg
for v in new old; do
    as_version $v
    config file_details=false
    start $d/wide.jpg
    sleep 2
    # The list's edge, at 240 by default, dragged to the window's left side.
    # Frames drawn as the pointer comes, so egui sees it over the edge before the press.
    for x in 250 245 241 239; do xdotool mousemove $x 450; sleep 0.2; done
    xdotool mousedown 1 2>/dev/null
    for x in $(seq 235 -5 20); do xdotool mousemove $x 450; sleep 0.05; done
    sleep 0.3
    xdotool mouseup 1 2>/dev/null
    sleep 1
    shot narrow-$v
    quit
    # A thumbnail is about 100 wide: at x=150 the list is still there, not the red view.
    if [ $v = new ]; then differ "the list keeps a thumbnail's width" "$(colour narrow-new 150 400)" red
    else note "old, at x=150: $(colour narrow-old 150 400)"; fi
done
as_version new

echo "-- the Pan view's calendar: a day with images shows its first one"
d=/tmp/calendar
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
convert -size 300x200 xc:blue $d/b.jpg
exiftool -q -overwrite_original "-DateTimeOriginal=2025:07:01 10:00:00" $d/a.jpg
exiftool -q -overwrite_original "-DateTimeOriginal=2025:07:16 10:00:00" $d/b.jpg
config
start $d/a.jpg
sleep 2
key ctrl+j; sleep 2
click 205 82 2   # EXIF date
click 140 82 2   # Calendar
shot calendar-new
quit
# July 2025 starts on a Tuesday: the 16th is the third Wednesday, with the blue picture.
expect "the calendar shows the day's first picture" "$(colour calendar-new 290 407)" blue

echo "-- the image info template: one whose fields are all empty shows no box"
d=/tmp/osd
mkdir -p $d
convert -size 1600x1000 xc:red $d/a.jpg
for t in empty:%comment% name:%name%; do
    config "osd_template=${t#*:}"
    start $d
    sleep 2
    key f
    sleep 1
    shot osd-${t%%:*}
    quit
done
# Red as it is without the box, darker under it.
for v in empty name; do
    r=$(red osd-$v 25 22)
    if [ $v = empty ] && [ $r -gt 230 ]; then pass "an empty template: no box"
    elif [ $v = name ] && [ $r -lt 160 ]; then pass "a template with the name: the box"
    else fail "the $v template: red $r"; fi
done

echo "-- printing 80 images of 3000x2000 to a PDF"
src=/tmp/print
mkdir -p $src
convert -size 3000x2000 xc: +noise Random -quality 92 /tmp/noise.jpg
for i in $(seq -w 1 80); do ln /tmp/noise.jpg $src/p$i.jpg; done
for v in new old; do
    as_version $v
    pdf=/tmp/print-$v.pdf
    # No thumbnails: their decoding would count as memory used by printing.
    config thumbnails=false
    start $src
    sleep 2
    before=$(hwm)
    answer $pdf; key ctrl+a; key ctrl+p; sleep 1; key Return
    last=-1
    for i in $(seq 300); do
        sleep 1
        s=$(stat -c %s $pdf 2>/dev/null || echo 0)
        [ "$s" -gt 0 ] && [ "$s" = "$last" ] && break
        last=$s
    done
    grow=$((($(hwm) - before) / 1024))
    quit
    pages=$(pdfinfo $pdf 2>/dev/null | awk '/^Pages/ {print $2}')
    if [ $v = new ]; then
        expect "a valid PDF of 80 pages" "$pages" "80"
        [ $grow -lt 300 ] && pass "memory grew $grow MB while printing" ||
            fail "memory grew $grow MB while printing (the PDF is $(du -m $pdf | cut -f1) MB)"
    else note "old: memory grew $grow MB"; fi
done

echo "-- screen profile: Adobe RGB, so sRGB red is shown less red"
as_version new
adobe=/usr/share/color/icc/colord/AdobeRGB1998.icc
d=/tmp/screen
mkdir -p $d
convert -size 600x400 xc:red $d/red.png
echo junk >/tmp/junk.icc
for profile in "" $adobe /tmp/junk.icc; do
    config "screen_profile=$profile"
    start $d
    sleep 1
    shot screen-$(basename "${profile:-none}" .icc)
    quit
done
expect "no screen profile: red as it is" "$(red screen-none 760 400)" "255"
r=$(red screen-AdobeRGB1998 760 400)
[ "$r" -lt 240 ] && pass "Adobe RGB screen: red shown as $r" || fail "Adobe RGB screen: red shown as $r"
expect "a file that is no profile: red as it is" "$(red screen-junk 760 400)" "255"

echo "-- the desktop's screen profile: a stand-in gdbus answers for colord with Adobe RGB"
cat >/tmp/stub/gdbus <<STUB
#!/bin/sh
case "\$*" in
*GetDevicesByKind*) echo "([objectpath '/org/freedesktop/ColorManager/devices/xrandr_1'],)" ;;
*"Device Profiles"*) echo "(<[objectpath '/org/freedesktop/ColorManager/profiles/icc_1']>,)" ;;
*Filename*) echo "(<'$adobe'>,)" ;;
*) exit 1 ;;
esac
STUB
chmod +x /tmp/stub/gdbus
for on in true false; do
    config "desktop_profile=$on"
    start $d
    sleep 2
    shot desktop-$on
    quit
done
rm /tmp/stub/gdbus
r=$(red desktop-true 760 400)
[ "$r" -lt 240 ] && pass "the desktop's Adobe RGB: red shown as $r" ||
    fail "the desktop's Adobe RGB: red shown as $r"
expect "the desktop's profile left out: red as it is" "$(red desktop-false 760 400)" "255"

echo "-- images without a profile, Adobe RGB from EXIF, and the image's own profile left out"
d=/tmp/input
mkdir -p $d
convert -size 600x400 'xc:rgb(60,160,60)' $d/plain.png
cp $d/plain.png $d/exif.png
exiftool -q -q -overwrite_original -ColorSpace=Uncalibrated -InteropIndex=R03 $d/exif.png
cp $d/plain.png $d/icc.png
exiftool -q -q -overwrite_original "-ICC_Profile<=$adobe" $d/icc.png
# show FILE SETTINGS…: the colour shown in the middle of the image.
show() {
    local file=$1
    shift
    config "$@"
    start $d/$file
    sleep 1
    shot input
    quit
    pixel input 760 400
}
plain=$(show plain.png)
expect "without a profile: as it is" "$plain" "60 160 60"
as_adobe=$(show plain.png input_profile=adobe)
differ "without a profile, taken as Adobe RGB" "$as_adobe" "$plain"
expect "EXIF says Adobe RGB (R03): shown as Adobe RGB" "$(show exif.png)" "$as_adobe"
expect "its own Adobe RGB profile: shown as Adobe RGB" "$(show icc.png)" "$as_adobe"
expect "its own profile left out: as sRGB" "$(show icc.png own_profile=false)" "$plain"

if [ -n "$QUICK" ]; then
    echo "-- QUICK: large folder and EXIF timings left out"
else
    echo "-- large folder: 500,000 hard links (ten files: ext4 links one at most 65,000 times)"
    big=/tmp/big
    mkdir -p $big
    convert -size 1200x800 plasma: /tmp/big-0.jpg
    python3 -c "
import os, shutil
for s in range(10):
    src = f'/tmp/big-{s}.jpg'
    if s: shutil.copy('/tmp/big-0.jpg', src)
    for i in range(s, 500000, 10):
        os.link(src, f'$big/{i}.jpg')"
    for run in 1 2; do for v in new old; do
        as_version $v
        config thumbnails=false
        start $big
        wait_first
        sleep 15
        note "$v run $run: $(first_image), RssAnon $(rss)"
        best big_ms_$v "$(first_image | grep -o '[0-9]*')"
        best big_mb_$v "$(rss | grep -o '[0-9]*')"
        quit
    done; done
    as_version new
    expect "first image within 1.5 times the last release's time" \
        "$(within $big_ms_new $big_ms_old 100)" yes
    expect "memory within 1.5 times the last release's" "$(within $big_mb_new $big_mb_old 20)" yes
    config thumbnails=false screen_profile=$adobe
    start $big
    wait_first
    sleep 15
    note "new with a screen profile: $(first_image), RssAnon $(rss)"
    quit

    # Work in the window's thread that grows with the list shows as a slow frame: the slowest
    # after the first image, printed at exit, while the whole list is selected and acted on.
    config thumbnails=false
    start $big
    wait_first
    key ctrl+a
    for i in $(seq 20); do xdotool key shift+Down; sleep 0.1; done
    key ctrl+d; key Escape
    key ctrl+r; key Escape
    key slash; xdotool type "mark:1"; sleep 1; key Escape
    quit
    slowest=$(grep -o 'slowest frame: [0-9]*' /tmp/glimt.err | grep -o '[0-9]*$')
    note "new: slowest frame $slowest ms, 500,000 selected"
    expect "no frame over 100 ms with 500,000 files selected" \
        "$([ "${slowest:-9999}" -le 100 ] && echo yes)" yes

    echo "-- sorting 3,000 JPEGs of about 1 MB by EXIF date"
    e=/tmp/exif-dates
    mkdir -p $e
    convert -size 1400x1000 xc: +noise Random -quality 85 /tmp/noise-1mb.jpg
    for i in $(seq -w 1 3000); do cp /tmp/noise-1mb.jpg $e/$i.jpg; done
    exiftool -q -q -overwrite_original \
        '-DateTimeOriginal<${FileName;s/\.jpg//;$_=sprintf("2020:01:01 %02d:%02d:%02d",$_/3600,$_/60%60,$_%60)}' $e
    cat $e/* >/dev/null   # read once, so the first run is not timing the disk
    for run in 1 2; do for v in new old; do
        as_version $v
        config sort=exif_original thumbnails=false
        start $e
        wait_first
        note "$v run $run: $(first_image)"
        best exif_ms_$v "$(first_image | grep -o '[0-9]*')"
        quit
    done; done
    as_version new
    expect "sorted within 1.5 times the last release's time" "$(within $exif_ms_new $exif_ms_old 100)" yes
fi

echo "-- not tested here, by hand: the Flatpak (two windows with Ctrl+N, --remote next to the"
echo "   focused one, --remote-all next to both, --remote-all quit)"
if [ $FAILED -gt 0 ]; then echo "$FAILED checks failed"; exit 1; fi
echo "every check passed"
