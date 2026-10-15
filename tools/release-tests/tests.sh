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
        libxrandr2 libxi6 libx11-xcb1 sqlite3 >/dev/null || { echo "FAIL installing the tools"; exit 1; }
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

echo "-- Rename answered after --remote next: nothing renamed, not the next image"
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
    if [ $v = new ]; then expect "every name kept" "$got" "a.png b.png c.png "
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

echo "-- the keyword tree: a box sets the keyword, with its place in the tree"
d=/tmp/keywords
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
printf 'Animals\nPlaces|Norway|Oslo\n' >$XDG_CONFIG_HOME/glimt/keywords
config exif_panel=true
start $d
sleep 2
# Frames drawn as the pointer moves: a panel that grows with each moves the boxes away.
for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
sleep 1
shot keywords-before
# The Animals box: on, then off again.
click 1056 310 2
shot keywords-on
on=$(cat $d/a.jpg.xmp 2>/dev/null | grep -c '<rdf:li>Animals</rdf:li>')
click 1056 310 2
off=$(cat $d/a.jpg.xmp 2>/dev/null | grep -c '<rdf:li>Animals</rdf:li>')
quit
expect "a box sets the keyword and its tree path" "$on" 2
expect "a box clears them again" "$off" 0

echo "-- a Lightroom catalog: its collection, keyword tree and stars imported"
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
INSERT INTO AgLibraryRootFolder VALUES (1, '$d/photos/');
INSERT INTO AgLibraryFolder VALUES (2, 1, '2024/');
INSERT INTO AgLibraryFile VALUES (3, 2, 'a', 'jpg'), (4, 2, 'b', 'jpg');
INSERT INTO Adobe_images VALUES (5, 3, 4.0), (6, 4, 3);
INSERT INTO AgLibraryCollection VALUES (7, 'com.adobe.ag.library.group', 'Travel', NULL),
    (8, 'com.adobe.ag.library.collection', 'Norway', 7);
INSERT INTO AgLibraryCollectionImage VALUES (9, 8, 5), (10, 8, 6);
INSERT INTO AgLibraryKeyword VALUES (11, NULL, NULL), (12, 'Places', 11), (13, 'Oslo', 12);
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
click 100 420 3   # File → Import Lightroom Catalog…
shot lightroom-dialog
click 382 447 2   # Next
shot lightroom-plan
click 454 462 2   # Import, keeping Glimt's stars
shot lightroom-done
quit
coll="$XDG_DATA_HOME/glimt/collections/Travel - Norway.gqv"
expect "the collection that was there gets the image it lacked, the other once" \
    "$(grep -c "^\"$d/photos/2024/[ab].jpg\"$" "$coll" 2>/dev/null)" 2
expect "the keyword under its parent" "$(grep -cx 'Places|Oslo' $XDG_CONFIG_HOME/glimt/keywords)" 1
stars=$(sort $XDG_STATE_HOME/glimt/marks | cut -f2 | tr '\n' ' ')
expect "Glimt's stars kept, the new ones taken" "$stars" "2 3 "
expect "the new stars in XMP too" "$(grep -c 'xmp:Rating="3"' $d/photos/2024/b.jpg.xmp)" 1

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
click 80 516   # File → Rename Collection…
key ctrl+a; xdotool type New; key Return
renamed=$(ls $c | tr '\n' ' ')
click 20 11
click 93 540 2   # File → Move Collection to Trash
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
        quit
    done; done
    as_version new
    config thumbnails=false screen_profile=$adobe
    start $big
    wait_first
    sleep 15
    note "new with a screen profile: $(first_image), RssAnon $(rss)"
    quit

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
        quit
    done; done
fi

echo "-- not tested here, by hand: the Flatpak (two windows with Ctrl+N, --remote next to the"
echo "   focused one, --remote-all next to both, --remote-all quit)"
if [ $FAILED -gt 0 ]; then echo "$FAILED checks failed"; exit 1; fi
echo "every check passed"
