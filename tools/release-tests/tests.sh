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

# run.sh runs it in steps (STEP): "prepare" installs and builds, once; "run" runs SCRIPT, a part
# of the checks, in each of several containers at once; "audit" runs cargo audit. Without STEP,
# all of it in one container, with every check (the screenshots and the quick scripts).
if [ "$(id -u)" = 0 ]; then
  if [ "$STEP" != run ] && [ "$STEP" != audit ]; then
    export DEBIAN_FRONTEND=noninteractive
    echo "-- installing tools"
    # A failed install (an unreachable mirror) stops here: without Xvfb every check would fail.
    apt-get update -qq >/dev/null
    apt-get install -y -qq xvfb xdotool x11-utils xclip imagemagick libimage-exiftool-perl ffmpeg \
        poppler-utils libglib2.0-bin colord-data python3 procps libgl1-mesa-dri libegl1 libgl1 \
        libxkbcommon-x11-0 libxcursor1 libxrandr2 libxi6 libx11-xcb1 sqlite3 shared-mime-info python3-nibabel python3-h5py python3-openslide python3-mrcfile python3-astropy python3-pydicom python3-gdcm libgdcm-tools default-jre-headless unzip dosfstools rclone fuse3 gvfs gvfs-fuse gvfs-backends openssh-server dbus python3-fusepy >/dev/null || { echo "FAIL installing the tools"; exit 1; }
    # Bio-Formats for its format is no Debian package: its release, checked against the hash
    # it had when added.
    curl -sfL -o /tmp/bftools.zip \
        https://downloads.openmicroscopy.org/bio-formats/8.5.0/artifacts/bftools.zip
    echo "07a3bb1d3de84da3a709655a1008cb2d9b19becc5bad4ae4112633aec9380478  /tmp/bftools.zip" \
        | sha256sum -c --quiet && (cd /opt && unzip -q /tmp/bftools.zip) \
        || echo "FAIL installing bftools"
    # tools/screenshots/run.sh (SHOTS=1) needs only this version.
    for v in new $([ -z "$SHOTS" ] && echo old); do
        # The last release built once: kept by its commit, which never changes.
        kept=/work-target/old-builds/$OLD_COMMIT
        if [ $v = old ] && [ -n "$OLD_COMMIT" ] && [ -x $kept ]; then
            echo "-- old: the build kept from an earlier run"
            cp $kept /opt/glimt-old
            continue
        fi
        echo "-- building $v"
        # With the click hook of the tests. Only the last release may be from before it had one:
        # NEW without it would turn every click by text into a failure, so its errors are shown.
        (cd /t/$v && export CARGO_TARGET_DIR=/work-target/release-tests-$v \
            && if [ $v = new ]; then cargo build -q --release --locked --features full,test-hooks
               else cargo build -q --release --locked --features full,test-hooks 2>/dev/null \
                   || cargo build -q --release --locked --features full; fi) || exit 1
        cp /work-target/release-tests-$v/release/glimt /opt/glimt-$v
        if [ $v = old ] && [ -n "$OLD_COMMIT" ]; then
            # Only the latest: a release a run is past is not needed again.
            rm -rf /work-target/old-builds && mkdir /work-target/old-builds && cp /opt/glimt-old $kept
        fi
    done
    [ "$STEP" = prepare ] && exit 0
  fi
  status=0
  if [ "$STEP" != audit ]; then
    useradd -m tester
    chmod 777 /t/out
    # A FAT file system, as on a camera card, for the check of case-only renames. Mounting it
    # needs a loop device, which run.sh gives only the part with that check, with --privileged.
    # Without one, that check fails and says so. The pattern does not match its own line.
    # Containers started at the same moment can take the same free loop device, and the one
    # that loses fails, so the mount is tried again a few times.
    if grep -q '^card=/mnt/[c]ard' "${SCRIPT:-/t/tests.sh}" \
        && truncate -s 16M /opt/card.img && mkfs.vfat -n CARD /opt/card.img >/dev/null \
        && mkdir -p /mnt/card; then
        for try in 1 2 3 4 5; do
            mount -o loop,uid=$(id -u tester),gid=$(id -g tester) /opt/card.img /mnt/card \
                2>/dev/null && break
            sleep 1
        done
    fi
    # The sftp:// folder of the check of network folders: an sshd on localhost that lets the
    # tester in with a key. ssh finds its keys in the home folder of /etc/passwd, not in $HOME.
    if grep -q '^fuse=[y]es' "${SCRIPT:-/t/tests.sh}"; then
        mkdir -p /run/sshd && ssh-keygen -A >/dev/null && /usr/sbin/sshd
        su tester -c 'mkdir -p ~/.ssh && ssh-keygen -q -t ed25519 -N "" -f ~/.ssh/id_ed25519 \
            && cp ~/.ssh/id_ed25519.pub ~/.ssh/authorized_keys \
            && ssh-keyscan -H localhost >~/.ssh/known_hosts 2>/dev/null'
    fi
    # The checks of a failing environment (faults=yes): a small tmpfs at /mnt/full, which they
    # fill, and at /mnt/lossy a FUSE pass-through of /opt/lossy whose unlink of a file named
    # lost-* removes it but answers EIO, and then answers stat of that name from a cache for 30
    # seconds, as a share whose reply was lost does (NFS, SMB, sshfs). Both need --privileged,
    # which run.sh gives the parts with these checks; without it those checks fail.
    if grep -q '^faults=[y]es' "${SCRIPT:-/t/tests.sh}"; then
        mkdir -p /mnt/full /mnt/lossy /opt/lossy && chown tester: /opt/lossy
        mount -t tmpfs -o size=16m,uid=$(id -u tester),gid=$(id -g tester) full /mnt/full
        cat >/opt/lossyfs.py <<'PY'
import errno, os, sys, time
from fusepy import FUSE, FuseOSError, Operations, fuse_get_context

STAT = ("st_mode", "st_nlink", "st_uid", "st_gid", "st_size", "st_atime", "st_mtime", "st_ctime")
STATFS = ("f_bavail", "f_bfree", "f_blocks", "f_bsize", "f_favail", "f_ffree", "f_files",
          "f_flag", "f_frsize", "f_namemax")

class Lossy(Operations):
    def __init__(self, root):
        self.root, self.ghosts = root, {}

    def __call__(self, op, *args):
        try:
            return super().__call__(op, *args)
        except OSError as e:
            raise FuseOSError(e.errno or errno.EIO)

    def r(self, path):
        return self.root + path

    def owned(self, path):
        uid, gid, _ = fuse_get_context()
        os.lchown(self.r(path), uid, gid)

    def getattr(self, path, fh=None):
        ghost = self.ghosts.get(path)
        if ghost and ghost[1] > time.time():
            return ghost[0]
        st = os.lstat(self.r(path))
        return {k: getattr(st, k) for k in STAT}

    def unlink(self, path):
        if os.path.basename(path).startswith("lost-"):
            st = self.getattr(path)
            os.unlink(self.r(path))
            self.ghosts[path] = (st, time.time() + 30)
            raise FuseOSError(errno.EIO)
        os.unlink(self.r(path))

    def link(self, target, source):
        os.link(self.r(source), self.r(target))

    def symlink(self, target, source):
        os.symlink(source, self.r(target))
        self.owned(target)

    def rename(self, old, new):
        os.rename(self.r(old), self.r(new))

    def create(self, path, mode, fi=None):
        fd = os.open(self.r(path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
        self.owned(path)
        return fd

    def mkdir(self, path, mode):
        os.mkdir(self.r(path), mode)
        self.owned(path)

    def open(self, path, flags):
        return os.open(self.r(path), flags)

    def read(self, path, size, offset, fh):
        return os.pread(fh, size, offset)

    def write(self, path, data, offset, fh):
        return os.pwrite(fh, data, offset)

    def truncate(self, path, length, fh=None):
        os.truncate(self.r(path), length)

    def flush(self, path, fh):
        return 0

    def fsync(self, path, datasync, fh):
        os.fsync(fh)

    def release(self, path, fh):
        os.close(fh)

    def readdir(self, path, fh):
        return [".", ".."] + os.listdir(self.r(path))

    def readlink(self, path):
        return os.readlink(self.r(path))

    def rmdir(self, path):
        os.rmdir(self.r(path))

    def chmod(self, path, mode):
        os.chmod(self.r(path), mode)

    def chown(self, path, uid, gid):
        os.lchown(self.r(path), uid, gid)

    def utimens(self, path, times=None):
        os.utime(self.r(path), times, follow_symlinks=False)

    def statfs(self, path):
        st = os.statvfs(self.r(path))
        return {k: getattr(st, k) for k in STATFS}

FUSE(Lossy(sys.argv[1]), sys.argv[2], foreground=True, allow_other=True,
     default_permissions=True)
PY
        python3 /opt/lossyfs.py /opt/lossy /mnt/lossy >/tmp/lossyfs.log 2>&1 &
        for try in $(seq 50); do grep -q ' /mnt/lossy fuse' /proc/mounts && break; sleep 0.1; done
    fi
    # -noreset: an X server starts over when its last client closes, and refuses every client
    # that comes in that moment. Each check starts and quits Glimt, so a window now and then got
    # XOpenDisplayFailed.
    su tester -c "Xvfb :98 -screen 0 1600x1000x24 -noreset >/tmp/xvfb.log 2>&1 &"
    sleep 2
    su tester -c "QUICK=$QUICK SHOTS=$SHOTS bash ${SCRIPT:-/t/tests.sh}"
    status=$?
    [ -n "$SHOTS" ] || [ "$STEP" = run ] && exit $status
  fi
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
# expect NAME GOT WANT. Two empty values are refused, as a helper that is gone or a command that
# printed nothing would otherwise pass any check. A check that wants nothing uses expect_empty.
expect() {
    if [ -z "$2$3" ]; then fail "$1: nothing to compare, both values are empty"
    elif [ "$2" = "$3" ]; then pass "$1"
    else fail "$1: got '$2', want '$3'"; fi
}
# expect_empty NAME GOT: passes only when GOT is empty.
expect_empty() { if [ -z "$2" ]; then pass "$1"; else fail "$1: got '$2', want nothing"; fi; }
# A command that is not found is a failed check, also inside $( ) or with its errors hidden. Bash
# runs this handler in a separate process, so it signals the script's own shell to count it.
NOT_FOUND=/tmp/commands-not-found
trap 'fail "a command not found: $(tail -n 1 $NOT_FOUND)"' USR1
command_not_found_handle() {
    echo "$1" >>$NOT_FOUND
    kill -USR1 $$
    return 127
}
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
# config SETTING=VALUE…: the settings file. An OLD before 0.74.0 or 0.80.0 reads only the old
# keys of the settings renamed then, so it gets those.
OLD_KEYS='s/^delete_to_trash=/trash=/; s/^slideshow_delay=/delay=/; s/^slideshow_random=/random=/;
    s/^use_image_profile=/own_profile=/; s/^list_videos=/videos=/; s/^info_panel=/exif_panel=/;
    s/^sort_case_sensitive=/sort_case=/; s/^search_case_sensitive=/search_case=/;
    s/^one_malloc_arena=/low_memory=/; s/^slideshow_fade=/fade=/; s/^icon_grid=/icons=/'
config() {
    if [ "$V" = old ]; then printf '%s\n' "$@" | sed "$OLD_KEYS"; else printf '%s\n' "$@"; fi \
        >$XDG_CONFIG_HOME/glimt/config
}
# The folder of the added formats' desktop files. OLD before 0.75.0 reads only its old name,
# decoders, so it gets that; NEW reads both, and the old one is still read for good.
formats_dir() { if [ "$V" = old ]; then echo $XDG_DATA_HOME/glimt/decoders; else echo $XDG_DATA_HOME/glimt/formats; fi; }
# The start of a section: its title, and NEW with nothing left by a section before, neither in
# the homes nor in /tmp/stub, as run.sh may start a part at any section. The working folder and
# the variables a section exports for itself are reset too; a new one of those goes in the list.
section() {
    echo "-- $1"
    cd /tmp
    unset GLIMT_BFTOOLS LANG FLATPAK_ID
    rm -rf /tmp/stub/* /tmp/home-new /tmp/home-old
    as_version new
}
# Set here, not in a section: run.sh may start a part at any section, so none may count on
# what one before it made. A section's own files are made by the section, or by a function
# here that makes them once (print_pictures).
adobe=/usr/share/color/icc/colord/AdobeRGB1998.icc
# 80 pictures of 3000x2000 in $src, for printing.
print_pictures() {
    src=/tmp/print
    [ -d $src ] && return
    mkdir -p $src
    convert -size 3000x2000 xc: +noise Random -quality 92 /tmp/noise.jpg
    for i in $(seq -w 1 80); do ln /tmp/noise.jpg $src/p$i.jpg; done
}
# The file or folder the stand-in file dialog answers with.
answer() { printf '#!/bin/sh\necho "%s"\n' "$1" >/tmp/stub/zenity; chmod +x /tmp/stub/zenity; }
# Runs glimt --remote ARGS and ends it after 30 seconds. When no window answers, because X
# refused it, `open` starts a window of its own and a query waits for an answer, and either one
# held a whole part for hours.
remote() { timeout 30 $BIN --remote "$@"; }
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
        tail -5 /tmp/glimt.err 2>/dev/null | sed 's/^/     /'
    fi
    # The first image, when there is one, before the keys come: under load (six parts at once)
    # a fixed wait is not enough, and a key before the list is read goes to no image.
    for i in $(seq 30); do grep -q "first image" /tmp/glimt.err 2>/dev/null && break; sleep 0.1; done
    sleep 1.5
    xdotool windowfocus $WIN 2>/dev/null
    xdotool mousemove 800 400 2>/dev/null
}
# The window is raised as well as focused, as a window manager does: Xvfb has none, and eframe
# runs no frame for a covered window, so a key sent to one would wait unread.
key() {
    xdotool windowraise $WIN 2>/dev/null; xdotool windowfocus $WIN 2>/dev/null
    xdotool key "$@" 2>/dev/null; sleep 0.6
}
# A click on each text in turn (a menu, then its item), by the text, not by a place that every
# change to the look would move: the test-hooks build's `--remote where` says where the text
# was drawn, and xdotool clicks there. A text NEW does not show fails, as every check after it
# would test nothing. The last release may lack the text or the hook, so for it this is a note.
menu() {
    for label in "$@"; do click_text 1 "$label"; sleep 0.5; done
}
# A right click on the text, as menu() clicks.
rclick() { click_text 3 "$1"; }
# where LABEL: where the text was drawn, as x y, or nothing when it is not on the screen.
where() { remote where "$1" 2>/dev/null | tr -d '\0'; }
# click_text BUTTON LABEL: the click of menu() and rclick().
click_text() {
    local at
    at=$(where "$2")
    case $at in
        [0-9]*) xdotool mousemove ${at% *} ${at#* } click $1 2>/dev/null ;;
        *) if [ "$V" = new ]; then fail "new: $2 is not on the screen"; else note "old: $2 is not on the screen (or no hook)"; fi ;;
    esac
}
click() { xdotool mousemove $1 $2 click 1 2>/dev/null; sleep ${3:-1}; }
# Closing waits for what is being written (a turned image, the settings): so does this, up to
# ten seconds, before the kill that ends a Glimt that does not listen.
quit() {
    timeout 30 $BIN --remote-all quit >/dev/null 2>&1
    for i in $(seq 50); do kill -0 $GPID 2>/dev/null || break; sleep 0.2; done
    kill $GPID 2>/dev/null
    wait $GPID 2>/dev/null
}
shot() { import -window root /t/out/$1.png; }
# A shot of a screen at rest: shots 0.5 s apart until two are the same, up to 10 seconds, as
# thumbnails come late. A check that the screen changed compares with this, not with any shot.
settled() {
    local i
    shot $1
    for i in $(seq 20); do
        sleep 0.5; shot $1-next
        [ "$(compare -metric AE /t/out/$1.png /t/out/$1-next.png null: 2>&1 | cut -d' ' -f1)" = 0 ] \
            && break
        mv /t/out/$1-next.png /t/out/$1.png
    done
    rm -f /t/out/$1-next.png
}
# Waits for a state instead of a fixed pause, which a loaded machine (six parts at once)
# outruns: until the command succeeds, up to $2 seconds (10), in steps of 0.1 s; false when
# the time is up, and the check after it then says what is wrong.
until_ok() { local n=$(( ${2:-10} * 10 )); until eval "$1" >/dev/null 2>&1; do n=$((n - 1)); [ $n -le 0 ] && return 1; sleep 0.1; done; }
until_current() { until_ok "[ \"\$(remote current 2>/dev/null | tr -d '\\0')\" = \"$1\" ]" ${2:-10}; }
until_file() { until_ok "[ -e \"$1\" ]" ${2:-10}; }
until_gone() { until_ok "[ ! -e \"$1\" ]" ${2:-10}; }
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
# matching NAME GEOMETRY EXPR: how many pixels of the screenshot NAME within GEOMETRY make the
# -fx expression EXPR true. ImageMagick 7's convert prints a line "list" before the answer
# when -fx is used, and 6 does not, so only the last line is the count.
matching() {
    convert /t/out/$1.png -crop $2 +repage -fx "$3" -format '%[fx:int(mean*w*h)]' info: |
        tail -1
}
# The pixels of a notice in the screenshot NAME: Glimt writes every notice in egui's light red
# (255,128,128) at the foot of the image area. The pictures of a check that looks for a notice
# have no such colour.
notice_pixels() { matching $1 1040x300+240+500 'r>0.85 && g>0.38 && g<0.62 && b>0.38 && b<0.62'; }
# until_notice NAME [SECONDS]: shots NAME until one shows a notice, up to SECONDS (10); false
# when none came.
until_notice() {
    local i
    for i in $(seq $(( ${2:-10} * 2 ))); do
        shot $1
        [ "$(notice_pixels $1)" -gt 20 ] && return 0
        sleep 0.5
    done
    return 1
}
# The red value, 0-255, at X,Y of a screenshot.
red() { convert /t/out/$1.png -format "%[fx:int(255*p{$2,$3}.r)]" info:; }
first_image() { grep -o 'first image: [0-9]* ms' /tmp/glimt.err; }
wait_first() { for i in $(seq 240); do grep -q "first image" /tmp/glimt.err && break; sleep 0.5; done; }
rss() { awk '/RssAnon/ {printf "%d MB", $2 / 1024}' /proc/$GPID/status; }
# The smaller of the variable named $1 and the number $2, kept in $1: the best of the runs, as one
# can be slow by chance. within: yes when $1 is at most 1.5 times $2, plus the slack $3 (ms or
# MB): a few hundred ms vary by more than half when something else runs on the machine.
best() { eval "best_seen=\${$1:-}"; { [ -z "$best_seen" ] || [ "${2:-999999}" -lt "$best_seen" ]; } && eval "$1=${2:-999999}"; }
# Never yes without a value of each version: a missing new one would compare as equal, and a
# missing old one (999999, its run gave no number) would let any new value pass the limit.
within() {
    [ "${1:-999999}" != 999999 ] && [ "${2:-999999}" != 999999 ] &&
        [ $(( $1 * 2 )) -le $(( $2 * 3 + ${3:-0} * 2 )) ] && echo yes
}
hwm() { awk '/VmHWM/ {print $2}' /proc/$GPID/status; }
# The control of a check that expects nothing to change: that the keys reach the window at all.
# t turns the thumbnails off, which is saved to the settings file; a key lost on the way leaves
# it as it was. (Full screen would not do: Xvfb has no window manager to resize the window.)
keys_arrive() {
    local before; before=$(md5sum <$XDG_CONFIG_HOME/glimt/config 2>/dev/null)
    key t; sleep 1
    [ "$(md5sum <$XDG_CONFIG_HOME/glimt/config 2>/dev/null)" != "$before" ] && echo yes
    key t; sleep 0.5
}
# Glimt's processor time over three seconds, in ticks of 1/100 s: near 0 when it waits.
cpu() { awk '{print $14 + $15}' /proc/$GPID/stat; }
busy() { local a=$(cpu); sleep 3; echo $(($(cpu) - a)); }
# A part of the shown image chosen for Crop (Shift+C): a drag from 800,200 to 1200,600.
crop_part() {
    xdotool mousemove 800 200 mousedown 1; sleep 0.3; xdotool mousemove 1000 400; sleep 0.3
    xdotool mousemove 1200 600; sleep 0.3; xdotool mouseup 1; sleep 0.5
}

cd /tmp

# tools/screenshots/run.sh: the pictures for the README and the website, instead of the checks.
if [ -n "$SHOTS" ]; then
    as_version new
    . /t/shots.sh
    exit $FAILED
fi

section "saving turned: PNG, lossless WebP and lossy WebP with EXIF orientation 6"
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

section "a turned lossy WebP keeps its ICC profile and pixels, also with colour management off"
for v in new old; do
    as_version $v
    for cm in true false; do
        d=/tmp/turned-icc-$v-$cm
        mkdir -p $d && cd $d
        convert -size 200x100 xc:'rgb(60,160,60)' -quality 90 a.webp
        exiftool -q -q -overwrite_original "-ICC_Profile<=$adobe" a.webp
        convert -size 20x20 xc:white z.png
        want="$(exiftool -s3 -ProfileDescription a.webp)"
        [ -n "$want" ] || fail "exiftool wrote no ICC profile into a.webp"
        cd /tmp
        config save_orientation=true color_management=$cm
        start $d
        key bracketright; key space
        sleep 6
        quit
        # The red of 60 rounds to 3 in twenties. Converted from Adobe RGB to sRGB it is about 0.
        got="$(identify -format '%wx%h' $d/a.webp) \
$(convert $d/a.webp -format '%[fx:int(p{50,100}.r*255/20+0.5)]' info:) \
$(exiftool -s3 -ProfileDescription $d/a.webp)"
        if [ $v = new ]; then
            expect "colour management $cm: turned, its profile kept, green as it was" \
                "$got" "100x200 3 $want"
        else note "old, colour management $cm: $got"; fi
    done
done

section "saves at once: a.png and a.webp of 6000x4000, turned one after the other"
d=/tmp/at-once
mkdir -p $d && cd $d
convert -size 3000x4000 xc:red -size 3000x4000 xc:blue +append a.png
convert a.png -define webp:lossless=true a.webp
cd /tmp
config save_orientation=true
start $d
key bracketright; key space; key bracketright; key space
for i in $(seq 60); do sleep 1; ls -A $d | grep -q glimt-tmp || break; done
sleep 2
quit
for f in a.png a.webp; do
    expect "$f: whole and turned" \
        "$(identify -format '%wx%h' $d/$f) $(side $d/$f 2000,10) $(side $d/$f 2000,5990)" \
        "4000x6000 red blue"
done
expect "no temporary files left" "$(ls -A $d | tr '\n' ' ')" "a.png a.webp "

section "a turn saved when another folder opens: to the turned image, not the new folder's"
for v in new old; do
    as_version $v
    d=/tmp/turn-open-$v
    mkdir -p $d/one $d/two
    convert -size 200x200 xc:red -size 200x200 xc:blue +append -depth 8 $d/one/a.png
    for f in one/b two/c two/d; do cp $d/one/a.png $d/$f.png; done
    config save_orientation=true
    start $d/one/b.png
    key bracketright
    timeout 30 $BIN --remote-all open $d/two >/dev/null 2>&1
    sleep 4
    quit
    got="$(cd $d && identify -format '%wx%h ' one/a.png one/b.png two/c.png two/d.png)"
    if [ $v = new ]; then
        expect "one/b.png turned, the rest as they were" "$got" "400x200 200x400 400x200 400x200 "
    else note "old: one/a one/b two/c two/d: $got"; fi
done

section "a JPEG turned and left: its orientation field saved, in the background"
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

section "Copy to Folder, then Move to Folder while its dialog is open: the copy is done, not a move"
for v in new old; do
    as_version $v
    d=/tmp/two-dialogs-$v
    mkdir -p $d/sub
    convert -size 100x100 xc:red $d/a.jpg
    printf '#!/bin/sh\nsleep 3\necho "%s"\n' $d/sub >/tmp/stub/zenity; chmod +x /tmp/stub/zenity
    config auto_refresh=false
    start $d/a.jpg
    key ctrl+c; key ctrl+m; until_file $d/sub/a.jpg; sleep 1
    quit
    got="$(ls $d | tr '\n' ' ')$(ls $d/sub | tr '\n' ' ')"
    if [ $v = new ]; then expect "a.jpg copied, still in its folder" "$got" "a.jpg sub a.jpg "
    else note "old: $got"; fi
done

section "a turned image renamed before its turn is saved: the turn saved under the new name"
for v in new old; do
    as_version $v
    d=/tmp/turn-rename-$v
    mkdir -p $d
    convert -size 200x200 xc:red -size 200x200 xc:blue +append -depth 8 $d/a.png
    convert -size 64x64 xc:lime $d/c.png
    config save_orientation=true
    start $d/a.png
    key bracketright                 # turned, not saved yet
    key ctrl+r; key ctrl+a; xdotool type b.png; key Return; sleep 1
    key space; sleep 3               # moving on saves the turn
    quit
    got="$(identify -format '%wx%h' $d/b.png 2>/dev/null)"
    if [ $v = new ]; then expect "b.png turned" "$got" "200x400"
    else note "old: b.png ${got:-missing}"; fi
done

section "an empty folder: the turn keys leave Glimt running"
for v in new old; do
    as_version $v
    d=/tmp/turn-empty-$v
    mkdir -p $d
    config
    start $d
    arrived=$(keys_arrive)
    key bracketright; key bracketleft; key shift+m
    alive=$(kill -0 $GPID 2>/dev/null && echo runs || echo ended)
    quit
    if [ $v = new ]; then
        expect "the keys reach the window (the control)" "$arrived" yes
        expect "Glimt still runs" "$alive" runs
    else note "old: $alive"; fi
done

section "an image that could not be read, put right by another program: read again when shown again"
d=/tmp/retry
mkdir -p $d
echo "not a picture" >$d/a.png
convert -size 600x400 xc:red $d/b.png
for v in new old; do
    as_version $v
    # No refresh of the folder, which would read everything again: the image itself must be.
    config auto_refresh=false
    start $d/a.png; sleep 2
    convert -size 600x400 xc:lime $d/a.png
    key Right; sleep 1; key Left; sleep 2
    shot retry-$v
    quit
    if [ $v = new ]; then expect "the image read again, not the old failure kept" "$(colour retry-$v 760 400)" green
    else note "old: $(colour retry-$v 760 400)"; fi
done

section "a turned image removed by another program: its turn goes to no other image"
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

section "subfolders, auto refresh off: copy and move to the folder and a subfolder"
d=/tmp/subfolders
mkdir -p $d/sub/deep && cd $d
convert -size 300x200 xc:red a.jpg
convert -size 300x200 xc:lime b.jpg
convert -size 300x200 xc:blue sub/c.jpg
convert -size 300x200 xc:yellow sub/d.jpg
cd /tmp
config auto_refresh=false
start $d
remote subfolders on; sleep 2
answer $d/sub; key ctrl+c; until_file $d/sub/a.jpg; sleep 1
key space; key space; key space; answer $d; key ctrl+m; until_file $d/c.jpg; sleep 1
key Home; key space; answer $d/sub/deep; key ctrl+m; until_file $d/sub/deep/b.jpg; sleep 1
shot subfolders
quit
expect "files on disk" "$(cd $d && find . -name '*.jpg' | sort | tr '\n' ' ')" \
    "./a.jpg ./c.jpg ./sub/a.jpg ./sub/d.jpg ./sub/deep/b.jpg "
# a.jpg, c.jpg, sub/a.jpg, sub/d.jpg, sub/deep/b.jpg
expect "the list follows" "$(rows subfolders 5)" "red blue red yellow green"

section "Include Subfolders saved on: on at the next start, off again by --remote subfolders off"
for v in new old; do
    as_version $v
    d=/tmp/subfolders-start-$v
    mkdir -p $d/sub
    for f in a b; do convert -size 64x64 xc:red $d/$f.jpg; done
    for f in c d e; do convert -size 64x64 xc:blue $d/sub/$f.jpg; done
    config show_subfolders=true
    start $d
    sleep 2
    key ctrl+a
    listed=$(remote selection 2>/dev/null | tr '\0' '\n' | grep -c .)
    remote subfolders off >/dev/null 2>&1; sleep 2
    key ctrl+a
    own=$(remote selection 2>/dev/null | tr '\0' '\n' | grep -c .)
    quit
    if [ $v = new ]; then
        expect "on at the start: all five" "$listed" 5
        expect "off by --remote: the folder's own two" "$own" 2
    else note "old: $listed, then $own"; fi
done

section "no busy loop: zoomed panes, a random slideshow behind a search or in panes one at a time"
# The slideshow changes its image every second, which costs some; the old loop costs thousands.
d=/tmp/busy
mkdir -p $d/panes $d/show
for i in 1 2 3 4; do convert -size 3000x2000 xc: +noise Random -quality 85 $d/panes/$i.jpg; done
for i in 1 2 3 4 5 6; do convert -size 64x64 xc:"rgb($((i * 40)),0,0)" $d/show/alpha$i.png; done
convert -size 64x64 xc:blue $d/show/omega.png
for v in new old; do
    as_version $v
    config layout=2x2
    start $d/panes/1.jpg; sleep 3
    key z; sleep 3
    panes=$(busy)
    quit
    # No fade: it draws every frame for its 400 ms, which is wanted, not a busy loop.
    config slideshow_random=true slideshow_repeat=true slideshow_delay=1 slideshow_fade=0
    start --slideshow $d/show; sleep 2
    key slash; xdotool type omega; sleep 2
    slideshow=$(busy)
    quit
    # Four panes, one image at a time, no fade: a new round shuffles once, then goes on.
    config slideshow_random=true slideshow_repeat=true slideshow_delay=1 slideshow_fade=0 layout=2x2 slideshow_one_image_at_a_time=2x2
    start --slideshow $d/show; sleep 4
    shot turns-a-$v
    turns=$(busy)
    shot turns-b-$v
    quit
    moved=$(for s in a b; do convert /t/out/turns-$s-$v.png -crop 1300x900+260+40 rgb:- | md5sum; done | uniq | wc -l)
    if [ $v = new ]; then
        expect "zoomed panes: Glimt at rest" "$([ $panes -lt 60 ] && echo yes)" yes
        expect "a random slideshow behind a search: Glimt at rest" "$([ $slideshow -lt 300 ] && echo yes)" yes
        expect "a random slideshow in four panes, one at a time: Glimt at rest" "$([ $turns -lt 300 ] && echo yes)" yes
        expect "... and its panes still change" "$moved" 2
    fi
    note "$v: ticks in 3 s: zoomed panes $panes, slideshow $slideshow, panes one at a time $turns (moved: $moved)"
done

section "a delete that fails in a subfolder: the file comes back"
for v in new old; do
    as_version $v
    d=/tmp/failed-delete-$v
    mkdir -p $d/sub && cd $d
    convert -size 300x200 xc:blue a.jpg
    convert -size 300x200 xc:red sub/b.jpg
    convert -size 300x200 xc:lime sub/c.jpg
    chmod 555 sub
    cd /tmp
    config auto_refresh=false confirm_delete=false
    start $d
    remote subfolders on >/dev/null 2>&1; sleep 2
    key space; key shift+Delete; sleep 3
    shot failed-delete-$v
    # A delete that can succeed, so the check above cannot pass by nothing happening.
    key Home; key shift+Delete; until_gone $d/a.jpg; sleep 1
    quit
    chmod 755 $d/sub
    # a.jpg, sub/b.jpg (not deleted), sub/c.jpg
    if [ $v = new ]; then
        expect "the file is in the list again" "$(rows failed-delete-new 3)" "blue red green"
        expect "a file that can go is deleted" "$(ls $d)" "sub"
    else note "old: $(rows failed-delete-old 3)"; fi
done

section "a delete that fails in a collection: the image stays in the collection"
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
    key Home; key shift+Delete; until_gone $d/a.jpg; sleep 1
    quit
    chmod 755 $d/ro
    got=$(grep -c 'ro/b.jpg' $d/show.gqv)
    if [ $v = new ]; then
        expect "b.jpg still in the collection" "$got" 1
        expect "a.jpg deleted" "$([ -e $d/a.jpg ] || echo gone)" gone
    else note "old: b.jpg lines in the collection: $got"; fi
done

section "a delete that fails while the same collection is opened again: the image comes back"
for v in new old; do
    as_version $v
    d=/tmp/reopen-delete-$v
    mkdir -p $d
    convert -size 300x200 xc:blue $d/a.jpg
    convert -size 300x200 xc:red $d/b.jpg
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n' $d $d >$d/show.gqv
    # A stand-in gio whose trash fails after 4 seconds: time to open the collection again.
    printf '#!/bin/sh\nsleep 4\necho "trash refused" >&2\nexit 1\n' >/tmp/stub/gio
    chmod +x /tmp/stub/gio
    config auto_refresh=false confirm_trash=false
    start $d/show.gqv
    sleep 2
    key ctrl+d                       # a.jpg to the trash: out of the list meanwhile
    remote open $d/show.gqv >/dev/null 2>&1
    sleep 1
    shown=$(remote current | tr -d '\0')
    during=$(grep -c 'a.jpg' $d/show.gqv)
    sleep 6
    quit
    rm /tmp/stub/gio
    got=$(grep -c 'a.jpg' $d/show.gqv)
    if [ $v = new ]; then
        # Held out of the list opened again while the delete runs (the control: the delete
        # was under way), but in the collection's file until it is really gone.
        expect "a.jpg held out of the collection opened again while it is deleted" "$shown" "$d/b.jpg"
        expect "a.jpg in the collection's file while the delete runs" "$during" 1
        expect "a.jpg in the collection after the delete failed" "$got" 1
    else note "old: shown $shown, a.jpg lines during $during, after $got"; fi
done

section "a slideshow one pane at a time, the folder read again: the second pane keeps its file"
for v in new old; do
    as_version $v
    d=/tmp/panes-refresh-$v
    mkdir -p $d
    convert -size 300x200 xc:red $d/a.png
    convert -size 300x200 xc:blue $d/b.png
    convert -size 300x200 xc:lime $d/c.png
    config layout=2x1 slideshow_one_image_at_a_time=2x1 slideshow_delay=60 auto_refresh=true
    start $d/a.png
    sleep 2
    key s; sleep 2                                  # a in the first pane, b in the second
    convert -size 300x200 xc:yellow $d/aa.png       # another program: now at place 1
    sleep 3
    shot panes-refresh-$v
    quit
    got=$(colour panes-refresh-$v 1020 410)
    if [ $v = new ]; then expect "the second pane still shows b.png" "$got" blue
    else note "old: $got"; fi
done

section "a file of the same name opened in another folder: not at the zoom of the one before"
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
    remote open $d/two/a.png >/dev/null 2>&1; until_current $d/two/a.png
    # A screen at rest: `current` is answered in the frame that opens two/a.png, before that
    # frame is drawn, so a shot at once still shows one/a.png zoomed in.
    settled same-name-$v
    quit
    got=$(colour same-name-$v 400 410)
    if [ $v = new ]; then expect "fitted, not zoomed" "$got" blue
    else note "old: $got"; fi
done

section "the folder read again while zoomed in: the zoom stays"
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

section "a refresh keeps the selection: a delete after it takes what was selected"
for v in new old; do
    as_version $v
    d=/tmp/refresh-selection-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    config confirm_delete=false
    start $d
    # a and b selected, then a file comes from elsewhere and auto refresh reads the folder.
    menu a.jpg; xdotool keydown ctrl; menu b.jpg; xdotool keyup ctrl
    cp $d/c.jpg $d/new.jpg; sleep 3
    shot refresh-selection-$v
    key shift+Delete; until_gone $d/a.jpg; until_gone $d/b.jpg; sleep 1
    quit
    left=$(ls $d | tr '\n' ' ')
    if [ $v = new ]; then expect "the selected files deleted" "$left" "c.jpg new.jpg "
    else note "old, left: $left"; fi
done

section "--remote current and selection: the paths, each ending in NUL, also with a line break"
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
    current=$(remote current | tr '\0' '|')
    menu a.jpg; xdotool keydown ctrl; menu b.jpg; xdotool keyup ctrl
    selection=$(remote selection | tr '\0' '|')
    # The name with a line break, shown: its path read whole, the file found by it.
    key End; sleep 1
    size=$(remote current | xargs -0 stat -c %s 2>/dev/null)
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

section "--remote info: every key of the image shown and of a file named, with its EXIF, keywords and stars"
# Before 0.78.0 the command did not exist, and the client refused it with exit status 2.
for v in new old; do
    as_version $v
    d=/tmp/remote-info-$v
    mkdir -p $d/other
    convert -size 300x200 xc:red $d/a.jpg
    exiftool -q -q -overwrite_original -Make=Canon "-Model=Canon EOS R6" -Orientation#=6 \
        "-DateTimeOriginal=2020:01:02 03:04:05" $d/a.jpg
    cat >$d/a.jpg.xmp <<'EOF'
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:subject><rdf:Bag><rdf:li>beach</rdf:li><rdf:li>sea</rdf:li></rdf:Bag></dc:subject>
<dc:description><rdf:Alt><rdf:li xml:lang="x-default">Sunset</rdf:li></rdf:Alt></dc:description>
</rdf:Description></rdf:RDF></x:xmpmeta>
EOF
    convert -size 40x30 xc:blue $d/other/b.png
    # Marks 1 and 4 (bits 9) and three stars.
    mkdir -p $XDG_STATE_HOME/glimt
    printf '9\t3\t%s\n' $d/a.jpg >$XDG_STATE_HOME/glimt/marks
    config auto_refresh=false
    start $d/a.jpg
    sleep 1
    shown=$(remote info 2>/dev/null)
    named=$(remote info $d/other/b.png 2>/dev/null)
    quit
    pick() { printf '%s\n' "$1" | awk -F '\t' -v keys="$2" \
        'BEGIN { n = split(keys, k, " "); for (i = 1; i <= n; i++) want[k[i]] = 1 }
         $1 in want { printf "%s=%s|", $1, $2 }'; }
    if [ $v = new ]; then
        expect "info: the keys, all and in order" "$(printf '%s\n' "$shown" | cut -f1 | tr '\n' ' ')" \
            "path size modified width height format orientation stars marks keywords comment alt_text extended_description exif.camera exif.lens exif.taken exif.exposure exif.aperture exif.iso exif.focal_length exif.exposure_bias exif.flash exif.gps "
        expect "info: the values of the image shown" \
            "$(pick "$shown" "path width height format orientation stars marks keywords comment exif.camera exif.taken")" \
            "path=$d/a.jpg|width=300|height=200|format=jpg|orientation=6|stars=3|marks=1,4|keywords=beach; sea|comment=Sunset|exif.camera=Canon EOS R6|exif.taken=2020:01:02 03:04:05|"
        expect "info: a file named that is not in the list" "$(pick "$named" "path width format stars")" \
            "path=$d/other/b.png|width=40|format=png|stars=0|"
    else note "old: '$shown'"; fi
done

section "--remote to a minimized window: answered at once, the image changes once it is shown"
# eframe runs no ui for a minimized window. In 0.76.0 before its fix, the window answered only
# in its ui, so every command waited 10 s for "no answer" and exited 1.
for v in new old; do
    as_version $v
    d=/tmp/remote-hidden-$v
    mkdir -p $d
    convert -size 1200x800 xc:red $d/a.png
    convert -size 1200x800 xc:blue $d/b.png
    config auto_refresh=false
    start $d/a.png
    # Minimized as a window manager marks it, which is what eframe reads. The test display has
    # no window manager, so the mark is set by hand, and the window is unmapped as well.
    xprop -id $WIN -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_HIDDEN
    xdotool windowunmap $WIN 2>/dev/null
    sleep 1
    t0=$(date +%s%N)
    remote next >/dev/null 2>&1
    status=$?
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    current=$(remote current 2>/dev/null | tr -d '\0')
    xprop -id $WIN -remove _NET_WM_STATE
    xdotool windowmap $WIN 2>/dev/null
    sleep 2
    xdotool windowfocus $WIN 2>/dev/null
    settled remote-hidden-$v
    shown=$(colour remote-hidden-$v 800 400)
    quit
    if [ $v = new ]; then
        expect "next to a minimized window: exit 0" "$status" 0
        expect "next answered at once (under 3 s: $ms ms)" "$([ $ms -lt 3000 ] && echo yes)" yes
        expect "current answered while minimized" "$current" "$d/b.png"
        expect "the next image is shown once the window is shown" "$shown" blue
    else note "old: exit $status in $ms ms, current '$current', shown $shown"; fi
done

section "--remote quit to a minimized window while copying: it waits, and ends once the copies are whole"
# eframe runs no ui for a minimized window. In 0.76.0 before its fix, a close there met no guard,
# so the window ended halfway through the copy.
for v in new old; do
    as_version $v
    d=/tmp/quit-hidden-$v
    mkdir -p $d/to
    # 240 names of one file: as much to copy as a fast machine needs to be still at it.
    head -c 25000000 /dev/urandom >$d/000.jpg
    for i in $(seq -w 1 239); do ln $d/000.jpg $d/$i.jpg; done
    config
    start $d
    sleep 2
    key ctrl+a
    answer $d/to
    key ctrl+c
    for i in $(seq 200); do [ -n "$(ls -A $d/to)" ] && break; sleep 0.05; done
    # Minimized as in the section before.
    xprop -id $WIN -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_HIDDEN
    xdotool windowunmap $WIN 2>/dev/null
    sleep 0.5
    remote quit >/dev/null 2>&1
    sleep 1
    waited=$(kill -0 $GPID 2>/dev/null && echo waits || echo closed)
    for i in $(seq 240); do kill -0 $GPID 2>/dev/null || break; sleep 0.5; done
    ended=$(kill -0 $GPID 2>/dev/null && echo runs || echo ended)
    quit
    whole=$(find $d/to -name '*.jpg' -size 24415k | wc -l)
    parts=$(ls -A $d/to | grep -c glimt-tmp)
    rm -rf $d    # 6 GB
    if [ $v = new ]; then
        expect "the minimized window waits for the copies" "$waited" waits
        expect "it ends once they are done, still minimized" "$ended" ended
        expect "all 240 copies whole, no hidden ones left" "$whole $parts" "240 0"
    else note "old: $waited, then $ended, $whole whole, $parts hidden"; fi
done

section "SIGTERM while moving to another disk: it waits, ends when every file is moved, marks follow"
# Before 0.78.0 a SIGTERM (a logout, kill) ended Glimt at once, halfway through the move, and the
# marks stayed on the names the moved files had before.
for v in new old; do
    as_version $v
    # From the tmpfs /dev/shm to the disk under /tmp, so each file is copied, not renamed.
    d=/dev/shm/sigterm-$v
    to=/tmp/sigterm-to-$v
    mkdir -p $d $to
    # 240 names of one file of 25 MB, as in the section before: 6 GB written to the disk.
    head -c 25000000 /dev/urandom >$d/000.jpg
    for i in $(seq -w 1 239); do ln $d/000.jpg $d/$i.jpg; done
    mkdir -p $XDG_STATE_HOME/glimt    # a part may start at this section
    for f in $d/*.jpg; do printf '1\t0\t%s\n' $f; done >$XDG_STATE_HOME/glimt/marks
    config
    start $d
    sleep 2
    key ctrl+a
    answer $to
    key ctrl+m
    for i in $(seq 200); do [ -n "$(ls -A $to)" ] && break; sleep 0.05; done
    kill -TERM $GPID
    sleep 1
    waited=$(kill -0 $GPID 2>/dev/null && echo waits || echo closed)
    for i in $(seq 240); do kill -0 $GPID 2>/dev/null || break; sleep 0.5; done
    ended=$(kill -0 $GPID 2>/dev/null && echo runs || echo ended)
    quit
    moved="$(find $to -name '*.jpg' -size 24415k | wc -l) $(ls -A $d | wc -l)"
    marks="$(grep -c "	$to/" $XDG_STATE_HOME/glimt/marks) $(grep -c "	$d/" $XDG_STATE_HOME/glimt/marks)"
    rm -rf $d $to    # 6 GB
    if [ $v = new ]; then
        expect "the window waits for the move" "$waited" waits
        expect "it ends once the files are moved" "$ended" ended
        expect "all 240 moved, none left behind" "$moved" "240 0"
        expect "the marks on the moved files, none on the old names" "$marks" "240 0"
    else note "old: $waited, then $ended, moved and left $moved, marks $marks"; fi
done

section "SIGHUP to a window at rest: it ends at once, and its state is written"
for v in new old; do
    as_version $v
    d=/tmp/sighup-$v
    mkdir -p $d
    convert -size 300x200 xc:red $d/a.jpg
    rm -f $XDG_STATE_HOME/glimt/state
    config
    start $d
    kill -HUP $GPID
    until_ok "! kill -0 $GPID" 5 && ended=ended || ended=runs
    state=$(grep -c . $XDG_STATE_HOME/glimt/state 2>/dev/null)
    quit
    rm -rf $d
    if [ $v = new ]; then
        expect "it ends within five seconds" "$ended" ended
        expect "the state file written" "$([ "${state:-0}" -gt 0 ] && echo written)" written
    else note "old: $ended, state lines ${state:-none}"; fi
done

section "thumbnails in the image area (G): ↓ moves a row, Enter shows the image, Esc the grid"
for v in new old; do
    as_version $v
    d=/tmp/area-grid-$v
    mkdir -p $d
    convert -size 300x200 xc:red /tmp/area-grid.jpg
    for i in $(seq -w 1 40); do ln -f /tmp/area-grid.jpg $d/f$i.jpg; done
    config auto_refresh=false
    start $d/f01.jpg
    n() { remote current | tr -d '\0' | sed 's/.*f0*\([0-9]*\)\.jpg/\1/'; }
    key g; key Down; row=$(n)
    key Return; key Down; one=$(n)
    key Escape; key Down; back=$(n)
    quit
    # The grid's row is its columns; the old release has no grid, and ↓ moves one image.
    cols=$((row - 1))
    if [ $v = new ]; then
        if [ "$cols" -gt 1 ]; then pass "↓ in the grid moves a row ($cols)"
        else fail "↓ in the grid moves a row: went to f$row"; fi
        expect "Enter shows the image: ↓ moves one" "$one" $((row + 1))
        expect "Esc goes back to the grid: ↓ moves a row" "$back" $((one + cols))
    else note "old: ↓ went to f$row, then f$one, f$back"; fi
done

section "the grid in the image area (G) with thumbnails and details on: the side panel has names"
for v in new old; do
    as_version $v
    d=/tmp/grid-names-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    config thumbnails=true file_details=true
    start $d
    key g
    # Thumbnails come late under load: a shot before them would find no red and pass.
    settled grid-names-$v
    quit
    # The side panel below the toolbar, left of the grid: red only where a thumbnail is drawn.
    reds=$(matching grid-names-$v 235x700+0+100 '(r>0.5&&g<0.3&&b<0.3)')
    # The control: the grid right of it has its red thumbnails, so they were drawn at all.
    grid_reds=$(matching grid-names-$v 600x500+300+100 '(r>0.5&&g<0.3&&b<0.3)')
    if [ $v = new ]; then
        expect "no thumbnails over the side panel's names" "$reds" 0
        expect "the grid has its thumbnails (the control)" "$([ "${grid_reds:-0}" -gt 0 ] && echo yes)" yes
    else note "old: $reds red pixels in the side panel"; fi
done

section "View → Thumbnails in Image Area: ticked while the grid shows, not on an image opened from it"
for v in new old; do
    as_version $v
    d=/tmp/grid-tick-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    config
    start $d
    key g; sleep 1
    menu View; sleep 1
    at_grid=$(where "Thumbnails in Image Area")
    shot grid-tick-grid-$v
    menu View; key Return; sleep 1        # the menu closed, then an image opened from the grid
    menu View; sleep 1
    at_image=$(where "Thumbnails in Image Area")
    shot grid-tick-image-$v
    quit
    # A ticked row is filled with the accent (blue, 53 132 228): count it along the row.
    ticked() {
        [ -n "$2" ] || { echo "not shown"; return; }
        local n
        n=$(matching $1 300x1+$(( ${2% *} - 150 ))+${2#* } '(b>0.7&&r<0.4)')
        [ "$n" -gt 100 ] && echo ticked || echo -
    }
    grid=$(ticked grid-tick-grid-$v "$at_grid"); image=$(ticked grid-tick-image-$v "$at_image")
    if [ $v = new ]; then
        expect "ticked while the grid shows" "$grid" ticked
        expect "not ticked on an image opened from the grid" "$image" -
    else note "old: $grid with the grid, $image on an image opened from it"; fi
done

section "View → Thumbnail Size → Large: the grid's thumbnails larger, and the size saved"
for v in new old; do
    as_version $v
    d=/tmp/grid-size-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    config
    start $d
    key g
    # The pointer off the grid, where no cell is lit and no name pops up over the thumbnails.
    xdotool mousemove 1270 790
    settled grid-size-small-$v
    menu View "Thumbnail Size" Large; sleep 1
    xdotool mousemove 1270 790
    settled grid-size-large-$v
    quit
    # The grid in the image area, right of the side panel: red only where a thumbnail is.
    red_in() { matching $1 1000x640+260+110 '(r>0.5&&g<0.3&&b<0.3)'; }
    small=$(red_in grid-size-small-$v); large=$(red_in grid-size-large-$v)
    saved=$(grep -c '^thumb_size=large$' $XDG_CONFIG_HOME/glimt/config)
    if [ $v = new ]; then
        if [ "$small" -gt 0 ] && [ "$large" -gt $((small * 2)) ]; then
            pass "the thumbnails larger: $small, then $large red pixels"
        else fail "the thumbnails larger: $small, then $large red pixels"; fi
        expect "Large saved" "$saved" 1
    else note "old: $small, then $large red pixels; saved $saved"; fi
done

section "View → Show File Names Under Thumbnails off: no names under the grid's thumbnails"
for v in new old; do
    as_version $v
    d=/tmp/grid-bare-$v
    mkdir -p $d
    for c in a b c d e f; do convert -size 300x200 xc:red $d/$c.jpg; done
    config
    start $d
    key g
    # The pointer off the grid, where no cell is lit and no name pops up over the thumbnails.
    xdotool mousemove 1270 790
    settled grid-bare-on-$v
    menu View "Show File Names Under Thumbnails"; sleep 1
    xdotool mousemove 1270 790
    settled grid-bare-off-$v
    saved=$(grep -c '^grid_names=false$' $XDG_CONFIG_HOME/glimt/config)
    quit
    # The names are light grey on the dark theme: grey pixels brighter than any background.
    # The thumbnails are red and the chosen cell blue, so neither counts.
    text_in() { matching $1 1000x640+260+110 '(r>0.4&&abs(r-g)<0.1&&abs(r-b)<0.1)'; }
    on=$(text_in grid-bare-on-$v); off=$(text_in grid-bare-off-$v)
    if [ $v = new ]; then
        if [ "$on" -gt 100 ]; then pass "names under the thumbnails at first ($on pixels)"
        else fail "names under the thumbnails at first: $on pixels"; fi
        expect "no names once turned off" "$off" 0
        expect "turned off and saved" "$saved" 1
    else note "old: $on, then $off pixels of text; saved $saved"; fi
done

section "Ctrl+ and Shift+double-click in the grid in the image area (G): open the file clicked"
for v in new old; do
    as_version $v
    d=/tmp/grid-ctrl-open-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    config
    start $d
    key g; sleep 1
    # The grid is drawn after the side panel, so where names its cells.
    at=$(where c.jpg)
    if [ -n "$at" ]; then
        xdotool mousemove ${at% *} ${at#* } keydown ctrl click --repeat 2 --delay 100 1 \
            keyup ctrl 2>/dev/null
        sleep 1
    fi
    current=$(remote current 2>/dev/null | tr -d '\0')
    key Escape; sleep 1                    # back to the grid; Shift+double-click on b
    at=$(where b.jpg)
    if [ -n "$at" ]; then
        xdotool mousemove ${at% *} ${at#* } keydown shift click --repeat 2 --delay 100 1 \
            keyup shift 2>/dev/null
        sleep 1
    fi
    shifted=$(remote current 2>/dev/null | tr -d '\0')
    quit
    if [ $v = new ]; then
        expect "Ctrl: the file double-clicked is shown" "$current" "$d/c.jpg"
        expect "Shift: the file double-clicked is shown" "$shifted" "$d/b.jpg"
    else note "old: $current shown after Ctrl, $shifted after Shift"; fi
done

section "Ctrl+ and Shift+double-click in the side panel's file list: open the file clicked"
for v in new old; do
    as_version $v
    d=/tmp/side-ctrl-open-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    config auto_refresh=false
    start $d/a.jpg
    at=$(where c.jpg)
    if [ -n "$at" ]; then
        xdotool mousemove ${at% *} ${at#* } keydown ctrl click --repeat 2 --delay 100 1 \
            keyup ctrl 2>/dev/null
        sleep 1
    fi
    current=$(remote current 2>/dev/null | tr -d '\0')
    at=$(where b.jpg)
    if [ -n "$at" ]; then
        xdotool mousemove ${at% *} ${at#* } keydown shift click --repeat 2 --delay 100 1 \
            keyup shift 2>/dev/null
        sleep 1
    fi
    shifted=$(remote current 2>/dev/null | tr -d '\0')
    quit
    if [ $v = new ]; then
        expect "Ctrl: the file double-clicked is shown" "$current" "$d/c.jpg"
        expect "Shift: the file double-clicked is shown" "$shifted" "$d/b.jpg"
    else note "old: $current shown after Ctrl, $shifted after Shift"; fi
done

section "--remote select while Delete asks: the answer deletes the file it asked about only"
for v in new old; do
    as_version $v
    d=/tmp/select-dialog-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    config confirm_delete=true
    start $d
    sleep 2
    key shift+Delete; sleep 1           # "Permanently delete a.png?"
    remote select $d/a.png $d/b.png $d/c.png >/dev/null 2>&1 &
    sleep 1
    key Return; sleep 2
    quit
    got="$(ls $d | tr '\n' ' ')"
    if [ $v = new ]; then expect "only a deleted" "$got" "b.png c.png "
    else note "old, left: $got"; fi
done

section "--remote open with a line break in the name: refused, not read as a second command"
for v in new old; do
    as_version $v
    d=/tmp/remote-newline-$v
    mkdir -p $d
    convert -size 64x64 xc:red $d/a.png
    config
    start $d
    sleep 2
    remote open "$d/x
quit" >/dev/null 2>&1; refused=$?
    sleep 1
    running=$(kill -0 $GPID 2>/dev/null && echo running || echo quit)
    quit
    if [ $v = new ]; then expect "refused, Glimt still running" "$refused $running" "2 running"
    else note "old: $refused $running"; fi
done

section "--remote slideshow on twice keeps it running, off twice keeps it stopped: a state, not a toggle"
d=/tmp/remote-on-off
mkdir -p $d
# Five images: the first slide comes at once, then one a second, so 2.5 s in it is on c or d.
for f in a b c d e; do convert -size 64x64 xc:red $d/$f.png; done
config slideshow_delay=1
start $d/a.png; sleep 2
remote slideshow on >/dev/null 2>&1; remote slideshow on >/dev/null 2>&1; sleep 2.5
moved=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
remote slideshow off >/dev/null 2>&1; remote slideshow off >/dev/null 2>&1; sleep 0.5
was=$(remote current 2>/dev/null | tr -d '\0'); sleep 2.5
still=$(remote current 2>/dev/null | tr -d '\0')
remote random on >/dev/null 2>&1; remote random on >/dev/null 2>&1; sleep 1
quit
# Empty answers are refused: a window that did not answer would pass both.
expect "on twice: running, past a.png" "$([ -n "$moved" ] && [ "$moved" != a.png ] && echo moved)" moved
expect "off twice: stopped, the image stays" "$([ -n "$was" ] && [ "$was" = "$still" ] && echo stays)" stays
expect "random on twice: on" "$(grep -c '^slideshow_random=true$' $XDG_CONFIG_HOME/glimt/config)" 1

section "--remote open of a folder whose name ends in a space: opened as named"
for v in new old; do
    as_version $v
    d=/tmp/remote-space-$v
    mkdir -p "$d/sub "
    convert -size 64x64 xc:red "$d/sub /a.png"
    convert -size 64x64 xc:blue $d/b.png
    config
    start $d
    sleep 2
    remote open "$d/sub " >/dev/null 2>&1; until_current "$d/sub /a.png"
    got=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    quit
    if [ $v = new ]; then expect "the folder's image shown" "$got" "sub /a.png"
    else note "old: $got"; fi
done

section "a selection the search hides part of: the status bar and the delete's question count the hidden files"
for v in new old; do
    as_version $v
    d=/tmp/part-hidden-$v
    mkdir -p $d
    for f in apple banana cherry; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d
    remote select $d/apple.png $d/banana.png $d/cherry.png >/dev/null 2>&1
    key slash; xdotool type cherry; sleep 2; key Escape
    status=$(where ", 2 selected but hidden by the search")
    key Delete; sleep 1
    asked=$(where "Not included, as the search hides them: 2 files")
    key Return; sleep 2
    quit
    got="$(ls $d | tr '\n' ' ')${status:+status} ${asked:+asked}"
    if [ $v = new ]; then expect "cherry deleted alone, the counts in the status bar and the question" "$got" "apple.png banana.png status asked"
    else note "old: $got"; fi
done

section "--remote run ACTION: an action for scripts runs, one that changes files is refused"
for v in new old; do
    as_version $v
    d=/tmp/remote-run-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    config confirm_trash=false
    start $d/a.png
    remote run Next >/dev/null 2>&1; sleep 1
    shown=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    remote run Delete >/dev/null 2>&1; refused=$?
    sleep 1
    quit
    got="$shown $refused $(ls $d | tr '\n' ' ')"
    if [ $v = new ]; then expect "b shown, Delete refused with 2, every file kept" "$got" "b.png 2 a.png b.png c.png "
    else note "old: $got"; fi
done

section "glimt --action-list: each action once with its label, and --remote run takes the first"
for v in new old; do
    as_version $v
    d=/tmp/action-list-$v
    mkdir -p $d
    convert -size 64x64 xc:red $d/a.png
    $BIN --action-list >$d/list 2>/dev/null; status=$?
    lines=$(grep -c . $d/list)
    names=$(cut -f1 $d/list | sort -u | grep -c .)
    labelled=$(grep -cP '^[A-Za-z0-9]+\t.+$' $d/list)
    first=$(head -1 $d/list | cut -f1)
    start $d/a.png
    remote run "$first" >/dev/null 2>&1; ran=$?
    quit
    got="$status $([ "$lines" -gt 50 ] && [ "$lines" = "$names" ] && [ "$lines" = "$labelled" ] && echo whole) $ran"
    if [ $v = new ]; then expect "exit 0, every line a name once with a label, the first one run" "$got" "0 whole 0"
    else note "old: $got"; fi
done

section "--remote run while a text field has the focus: not done, exit 3 and the reason"
for v in new old; do
    as_version $v
    d=/tmp/remote-not-done-$v
    mkdir -p $d
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d/a.png; sleep 1
    remote run FocusSearch >/dev/null 2>&1; sleep 1
    remote run Next >/dev/null 2>/tmp/remote-not-done-$v.err; status=$?
    sleep 1
    shown=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    quit
    told=$(grep -c "not done" /tmp/remote-not-done-$v.err)
    got="$status $told $shown"
    if [ $v = new ]; then expect "Next not done: exit 3, the reason told, a.png still shown" "$got" "3 1 a.png"
    else note "old: $got"; fi
done

section "--remote previous, and the words from before 0.74.0: prev and run Prev still taken"
d=/tmp/remote-previous
mkdir -p $d
for f in a b c d; do convert -size 64x64 xc:red $d/$f.png; done
config
start $d/d.png; sleep 2
remote previous >/dev/null 2>&1; sleep 1
after_previous=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
remote prev >/dev/null 2>&1; sleep 1
after_prev=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
remote run Prev >/dev/null 2>&1; sleep 1
after_run=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
quit
expect "previous, prev and run Prev each one image back" "$after_previous $after_prev $after_run" \
    "c.png b.png a.png"

section "--remote select while Preferences is open: answered at once"
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
    remote select $d/b.png >/dev/null 2>&1
    took=$(( $(date +%s) - start_at ))
    got=$(remote selection 2>/dev/null | tr '\0' ' ' | sed "s|$d/||g")
    quit
    if [ $v = new ]; then expect "b selected at once" "$got $([ $took -lt 5 ] && echo soon)" "b.png  soon"
    else note "old: '$got', ${took}s"; fi
done

section "--remote select through a linked folder: the files are found"
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
    missing=$(remote select $d/link/b.png $d/link/c.png 2>/dev/null | tr '\0' ' ')
    got=$(remote selection 2>/dev/null | tr '\0' ' ' | sed "s|$d/real/||g")
    quit
    if [ $v = new ]; then expect "b and the link c selected, none missing" "$got|$missing" "b.png c.png |"
    else note "old: $got|$missing"; fi
done

section "--remote select: right after open, files of the list selected, the others printed"
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
    remote open $d
    missing=$(remote select $d/b.jpg "$d/c d.jpg" /nowhere/e.jpg | tr '\0' '|')
    sleep 1
    selection=$(remote selection | tr '\0' '|')
    current=$(remote current | tr '\0' '|')
    quit
    if [ $v = new ]; then
        expect "select: the file not in the list printed" "$missing" "/nowhere/e.jpg|"
        expect "select: the others selected" "$selection" "$d/b.jpg|$d/c d.jpg|"
        expect "select: the first shown" "$current" "$d/b.jpg|"
    else note "old: '$missing' '$selection'"; fi
done

section "copying 240 files of 25 MB, closed halfway: counted in the status bar, every copy whole"
for v in new old; do
    as_version $v
    d=/tmp/progress-$v
    mkdir -p $d/to
    # 240 names of one file: as much to copy as a fast machine needs to be still at it.
    head -c 25000000 /dev/urandom >$d/000.jpg
    for i in $(seq -w 1 239); do ln $d/000.jpg $d/$i.jpg; done
    config
    start $d
    sleep 2
    key ctrl+a
    answer $d/to
    key ctrl+c
    # The copy starts once the stand-in file dialog has answered; closed as it begins.
    for i in $(seq 200); do [ -n "$(ls -A $d/to)" ] && break; sleep 0.05; done
    xdotool windowfocus $WIN key ctrl+q 2>/dev/null; sleep 0.3
    shot progress-$v
    waited=$(kill -0 $GPID 2>/dev/null && echo waits || echo closed)
    for i in $(seq 240); do kill -0 $GPID 2>/dev/null || break; sleep 0.5; done
    quit
    whole=$(find $d/to -name '*.jpg' -size 24415k | wc -l)
    parts=$(ls -A $d/to | grep -c glimt-tmp)
    rm -rf $d    # 6 GB
    if [ $v = new ]; then
        expect "the window waits for the copies" "$waited" waits
        expect "all 240 copies whole, no hidden ones left" "$whole $parts" "240 0"
    else note "old: $waited, $whole whole, $parts hidden"; fi
done

section "copying, closed twice: stopped after the file at hand, no hidden half copy left"
for v in new old; do
    as_version $v
    d=/tmp/close-twice-$v
    mkdir -p $d/to
    # 240 names of one file: as much to copy as a fast machine needs to be still at it.
    head -c 25000000 /dev/urandom >$d/000.jpg
    for i in $(seq -w 1 239); do ln $d/000.jpg $d/$i.jpg; done
    config
    start $d
    sleep 2
    key ctrl+a
    answer $d/to
    key ctrl+c
    for i in $(seq 200); do [ -n "$(ls -A $d/to)" ] && break; sleep 0.05; done
    key ctrl+q; key ctrl+q
    for i in $(seq 120); do kill -0 $GPID 2>/dev/null || break; sleep 0.5; done
    quit
    parts=$(ls -A $d/to | grep -c glimt-tmp)
    copied=$(ls $d/to | grep -c jpg)
    whole=$(find $d/to -name '*.jpg' -size 24415k | wc -l)
    rm -rf $d    # 6 GB
    if [ $v = new ]; then
        expect "no hidden half copy, every copy whole" "$parts $([ $copied = $whole ] && echo whole)" "0 whole"
        expect "copied some, stopped before all 240" "$([ $copied -ge 1 ] && [ $copied -lt 240 ] && echo yes)" yes
    else note "old: $parts hidden, $copied copied, $whole whole"; fi
done

section "thumbnails with Auto Rotate off"
# Its own pictures: run.sh may run this section in a part without the ones before it.
d=/tmp/thumbs
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
convert -size 300x200 xc:lime $d/b.jpg
rm -rf $XDG_CACHE_HOME/thumbnails
config exif_rotate=false
start $d
sleep 3
quit
expect "none saved to the shared cache" "$(find $XDG_CACHE_HOME/thumbnails -type f 2>/dev/null | wc -l)" "0"
# With it on they are saved: the check above cannot pass by no thumbnails being made.
config exif_rotate=true
start $d
sleep 3
quit
n=$(find $XDG_CACHE_HOME/thumbnails -type f 2>/dev/null | wc -l)
[ $n -gt 0 ] && pass "with Auto Rotate on, $n saved" || fail "with Auto Rotate on, none saved"

section "stars follow a renamed file, and go with a deleted one"
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
    key Home; key shift+Delete; until_gone $d/b.png; sleep 1    # b, first after the rename
    quit
    got="$(sort $XDG_STATE_HOME/glimt/marks | tr '\t\n' '  ')"
    if [ $v = new ]; then expect "the four stars on x, none left for b" "$got" "0 4 $d/x "
    else note "old: $got"; fi
done

section "stars that cannot be saved (a folder that cannot be written): told, not lost silently"
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

section "a mark set by another window meanwhile: a mark's key here goes from it"
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

section "stars to XMP for two files, the first one's sidecar not writable: the second still gets them"
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

section "Rename answered after the folder was read again: the same file renamed"
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

section "Rename with one file selected by a right-click: that file renamed, not the one shown"
for v in new old; do
    as_version $v
    d=/tmp/rename-selected-$v
    mkdir -p $d
    for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d/a.png
    sleep 2
    rclick b.png; sleep 1                        # b.png's row: selected, its menu opens
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

section "Rename answered after --remote next: the file asked about renamed, not the next image"
for v in new old; do
    as_version $v
    d=/tmp/rename-next-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    config
    start $d
    key ctrl+r
    timeout 30 $BIN --remote-all next >/dev/null 2>&1
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

section "Rename Many of 20,000 in a collection, an image taken out while it runs: the others stay"
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

section "Rename Many, closed at once: the window waits, every file renamed, no plan left"
for v in new old; do
    as_version $v
    d=/tmp/rename-close-$v
    mkdir -p $d
    convert -size 64x64 xc:red $d/00000.jpg
    for i in $(seq -w 1 9999); do ln $d/00000.jpg $d/$i.jpg; done
    config
    start $d
    sleep 2
    key ctrl+a; key ctrl+r; sleep 1
    key Return                         # Rename Many as *_###, in the background
    # Closed as soon as the first file has its new name: the window must wait for the rest.
    for i in $(seq 100); do ls $d | grep -q '_[0-9]*\.jpg' && break; sleep 0.1; done
    timeout 30 $BIN --remote-all quit >/dev/null 2>&1
    quit
    got="$(ls $d | grep -c '_[0-9]*\.jpg') $(ls $d | grep -vc '_[0-9]*\.jpg') $(ls $XDG_STATE_HOME/glimt/renames 2>/dev/null | wc -l | tr -d ' ')"
    if [ $v = new ]; then expect "10,000 renamed, none left, no plan" "$got" "10000 0 0"
    else note "old: renamed, not renamed, plans: $got"; fi
done

section "Rename Many failing halfway: names that swap, and one too long"
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

section "Rename Many on a FAT card: a change of case and a swap, through the hidden names; a Copy To the card, also through a link"
# A real vfat, where IMG_1.JPG and img_1.JPG are one name. A hard link on ext4 or a faked
# listing (#47) cannot show what the kernel's FAT does, so without the mount this fails.
card=/mnt/card
if ! grep -q " $card vfat " /proc/mounts; then
    fail "no FAT file system at $card: run.sh's containers need --privileged for a loop device"
else
    rm -rf $card/* $card/.glimt-rename-*
    colours=(red green blue)
    for n in 1 2 3; do
        convert -size 64x64 xc:${colours[n - 1]} $card/IMG_$n.JPG
        echo "sidecar $n" >$card/IMG_$n.JPG.xmp
    done
    # What each file holds, to follow it to its new name.
    contents_and_sidecar() { echo "$(md5sum <$1 | cut -c1-32) $(cat $1.xmp)"; }
    want=$(for n in 3 2 1; do contents_and_sidecar $card/IMG_$n.JPG; done)
    # Sorted by name, descending: IMG_3 becomes img_1, IMG_2 img_2 (its case only) and IMG_1
    # img_3, which takes the name IMG_3 had on FAT.
    config sort_ascending=false
    start $card
    key ctrl+a; key ctrl+r; sleep 1; key ctrl+a
    xdotool type --delay 30 'img_#'; sleep 0.4
    key Return; sleep 2
    quit
    expect "the new names, and no hidden one left" "$(ls -A $card | tr '\n' ' ')" \
        "img_1.JPG img_1.JPG.xmp img_2.JPG img_2.JPG.xmp img_3.JPG img_3.JPG.xmp "
    expect "each image and its sidecar kept, under its new name" \
        "$(for n in 1 2 3; do contents_and_sidecar $card/img_$n.JPG 2>/dev/null; done)" "$want"
    # A Copy To the card of a file whose name FAT refuses copies nothing, and says why before
    # it starts. 0.62.0 copied the others and then failed on that one with the system's error.
    d=/tmp/fat-copy
    rm -rf $d; mkdir -p $d
    for f in a 12:30; do convert -size 64x64 xc:red "$d/$f.jpg"; done
    config
    start $d
    key ctrl+a; answer $card; key ctrl+c; sleep 2; shot fat-copy
    quit
    expect "a Copy To the card with a name FAT refuses copies nothing" "$(ls $card | grep -c '^a.jpg')" 0
    # The same through a link to the card, as ~/card → /media/u/CARD. 0.63.0 looked the mount
    # up by the link's own path, so it copied a.jpg and then failed on 12:30.jpg.
    ln -sfn $card /tmp/card-link
    start $d
    key ctrl+a; answer /tmp/card-link; key ctrl+c; sleep 2; shot fat-copy-link
    quit
    expect "a Copy To the card through a link with a name FAT refuses copies nothing" \
        "$(ls $card | grep -c '^a.jpg')" 0
    # The control: a Copy To of a.jpg alone, the image shown, lands on the card, so a Copy To
    # that copies nothing at all cannot pass the two checks above.
    start $d/a.jpg
    answer /tmp/card-link; key ctrl+c; sleep 2
    quit
    expect "a Copy To the card of a name FAT allows lands there" "$(ls $card | grep -c '^a.jpg$')" 1
fi

section "the usual operations on an rclone mount and on an sftp:// folder Glimt mounts with gio"
# Network and cloud folders through FUSE (#30). rclone with its local backend has neither
# RENAME_NOREPLACE nor hard links, so a rename there is the fallback that looks first. gvfsd-fuse,
# where Glimt's own smb:// and sftp:// folders are, has RENAME_NOREPLACE but no hard links. Both
# need /dev/fuse, which run.sh gives this part with --privileged.
fuse=yes
# on_mount NAME DIR [START LOCAL]: Rename, stars, a keyword, Move and its Undo, the trash, and
# Rename Many in DIR, which START (default DIR) opens. The pictures are made in LOCAL
# (default DIR), the folder on the server, as DIR is there only once Glimt has mounted it.
on_mount() {
    local name=$1 d=$2 at=${4:-$2}
    mkdir -p $at/sub
    for f in a b c; do convert -size 64x64 xc:red $at/$f.jpg; done
    printf 'Animals\n' >$XDG_CONFIG_HOME/glimt/keywords
    config confirm_trash=false confirm_delete=false xmp_stars=true info_panel=true
    start ${3:-$d}
    until_current $d/a.jpg 30
    key ctrl+r; key ctrl+a; xdotool type --delay 30 r.jpg; key Return
    until_file $d/r.jpg 20; shot fs-$name-rename
    expect "$name: Rename" "$(ls $d | tr '\n' ' ')" "b.jpg c.jpg r.jpg sub "
    key alt+3; until_file $d/r.jpg.xmp 20
    expect "$name: stars in the sidecar" "$(grep -c 'Rating="3"\|Rating>3<' $d/r.jpg.xmp 2>/dev/null)" 1
    for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done; sleep 1
    menu Animals; sleep 3
    expect "$name: a keyword in the sidecar" "$(grep -c '<rdf:li>Animals</rdf:li>' $d/r.jpg.xmp 2>/dev/null)" 2
    xdotool mousemove 800 400
    answer $d/sub; key ctrl+m; until_file $d/sub/r.jpg 20; sleep 1; shot fs-$name-move
    expect "$name: Move with its sidecar" "$(ls $d/sub | tr '\n' ' ')" "r.jpg r.jpg.xmp "
    key ctrl+z; until_file $d/r.jpg 20; sleep 1
    expect "$name: the move undone" "$(ls $d | tr '\n' ' ')" "b.jpg c.jpg r.jpg r.jpg.xmp sub "
    # Neither has a trash gio can use: rclone answers it with an input/output error, gvfsd-fuse
    # with "Operation not supported". Glimt asks whether to delete the file for good instead.
    key Home; key ctrl+d; sleep 4; shot fs-$name-trash
    menu "Delete (Enter)"; until_gone $d/b.jpg 20; sleep 1
    expect "$name: the trash refused, deleted for good when asked" "$(ls $d | tr '\n' ' ')" \
        "c.jpg r.jpg r.jpg.xmp sub "
    key ctrl+a; key ctrl+r; sleep 1; key ctrl+a
    xdotool type --delay 30 'n_#'; sleep 0.4
    key Return; until_file $d/n_2.jpg 20; sleep 2; shot fs-$name-rename-many
    quit
    expect "$name: Rename Many with the sidecar" "$(ls $d | tr '\n' ' ')" \
        "n_1.jpg n_2.jpg n_2.jpg.xmp sub "
    note "$name: $(grep -c . $XDG_STATE_HOME/glimt/marks 2>/dev/null) stars kept in marks"
}
mkdir -p /tmp/rclone-back /tmp/rclone
rclone mount --daemon /tmp/rclone-back /tmp/rclone 2>/dev/null
if ! grep -q " /tmp/rclone fuse.rclone " /proc/mounts; then
    fail "no rclone mount: run.sh's containers need --privileged for /dev/fuse"
else
    on_mount rclone /tmp/rclone/photos
    fusermount3 -uz /tmp/rclone
fi
# gio needs a session bus. gvfsd-fuse, which the bus starts, mounts under this XDG_RUNTIME_DIR.
bus=$(dbus-daemon --session --fork --print-address=1 --print-pid=1 --nopidfile)
export DBUS_SESSION_BUS_ADDRESS=$(echo "$bus" | head -1)
if ! timeout 20 ssh -o BatchMode=yes localhost true; then
    fail "no sshd on localhost for the sftp:// folder"
else
    on_mount sftp "$XDG_RUNTIME_DIR/gvfs/sftp:host=localhost$HOME/sftp/photos" \
        "sftp://localhost$HOME/sftp/photos" $HOME/sftp/photos
fi
grep fuse /proc/mounts | sed 's/^/     /'
gio mount -u "sftp://localhost/" 2>/dev/null
# -z: a busy mount (a hung gvfsd-sftp) is let go of all the same, so no later rm walks into it.
fusermount3 -uz $XDG_RUNTIME_DIR/gvfs 2>/dev/null
kill $(echo "$bus" | tail -1) 2>/dev/null
unset DBUS_SESSION_BUS_ADDRESS

section "Rename Many with Include Subfolders: the view stays on the folder and its subfolders"
for v in new old; do
    as_version $v
    d=/tmp/rename-subfolders-$v
    mkdir -p $d/sub
    convert -size 100x100 xc:blue $d/c.jpg
    convert -size 100x100 xc:red $d/sub/a.jpg
    convert -size 100x100 xc:lime $d/sub/b.jpg
    config auto_refresh=false
    start $d
    remote subfolders on >/dev/null 2>&1; sleep 2
    key End                       # sub/b.jpg shown, so it is among the renamed
    key ctrl+a; key ctrl+r; sleep 1; key ctrl+a
    xdotool type --delay 30 'x*'; sleep 0.4
    key Return; sleep 3
    key End; sleep 1              # the folder's own file sorts last, after sub/
    got=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    quit
    if [ $v = new ]; then expect "the folder's own file still in the list" "$got" "xc.jpg"
    else note "old, last: $got"; fi
done

section "Find Duplicates: trash the checked files"
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
    remote open $d/other/x.jpg; sleep 2
    click 459 129 3   # "Move 2 checked to trash…"
    shot duplicates-$v
    quit
    trash=$(ls $H/.local/share/Trash/files 2>/dev/null | tr '\n' ' ')
    if [ $v = new ]; then expect_empty "the list changed: nothing in the trash" "$trash"
    else note "old, in the trash: $trash"; fi
done
as_version new
rm -rf $H/.local/share/Trash
start /tmp/duplicates-new
key d; click 525 82 2; click 147 129; click 459 129 3
quit
expect "the list as it was: the checked in the trash" \
    "$(ls $H/.local/share/Trash/files 2>/dev/null | tr '\n' ' ')" "b.jpg c.jpg "

section "pan view: another sort order keeps each date's image"
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

section "pan view far from the file list's rows: its cells get thumbnails"
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

section "Escape closes the pan view"
d=/tmp/pan-escape
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
config
start $d
sleep 1; shot pan-esc-before
key ctrl+j; sleep 2; shot pan-esc-open
key Escape; sleep 1.5; shot pan-esc-after
quit
same() { md5sum </t/out/$1.png; }
differ "the pan view opened" "$(same pan-esc-before)" "$(same pan-esc-open)"
expect "closed by Escape" "$(same pan-esc-after)" "$(same pan-esc-before)"

section "a video that is a playlist: ffmpeg opens no file it names"
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

section "a video opened with videos off, then Open Several Folders: the folders list no videos"
for v in new old; do
    as_version $v
    d=/tmp/opened-type-$v
    mkdir -p $d/one $d/two
    ffmpeg -v error -f lavfi -i color=c=blue:s=320x240:d=1 -pix_fmt yuv420p $d/v.mp4
    cp $d/v.mp4 $d/one/w.mp4
    convert -size 64x64 xc:red $d/one/a.png
    convert -size 64x64 xc:red $d/two/b.png
    printf '#!/bin/sh\nprintf "%%s\\n%%s\\n" %s %s\n' $d/one $d/two >/tmp/stub/zenity; chmod +x /tmp/stub/zenity
    config list_videos=false
    start $d/v.mp4; sleep 2
    menu File "Open Several Folders…"; sleep 3
    key ctrl+a
    listed=$(remote selection 2>/dev/null | tr '\0' '\n' | sed "s|$d/||" | sort | tr '\n' ' ')
    quit
    if [ $v = new ]; then expect "the two images only, no video" "$listed" "one/a.png two/b.png "
    else note "old: $listed"; fi
done

section "added formats: the examples show a .jp2 through ffmpeg and a .fits through astropy"
for v in new old; do
    as_version $v
    d=/tmp/decoders-$v
    mkdir -p $d $(formats_dir)
    cp /t/new/examples/formats/* $(formats_dir)/
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
        expect "a .jp2 through its format" "$(colour decoder-jp2-new 640 400)" red
        top=$(red decoder-fits-new 640 120) bottom=$(red decoder-fits-new 640 700)
        expect "a .fits through its format, stretched" "$([ $top -lt 60 ] && [ $bottom -gt 190 ] && echo yes)" yes
        top=$(red decoder-fz-new 760 120) bottom=$(red decoder-fz-new 760 700)
        expect "a compressed .fits.fz, its top at the top" "$([ $top -gt 190 ] && [ $bottom -lt 60 ] && echo yes)" yes
    else note "old: $(colour decoder-jp2-old 640 400), fits $(red decoder-fits-old 640 700)"; fi
done

section "the ImageMagick fallbacks: a .fits and a .dcm without astropy or pydicom's decoders"
for v in new old; do
    as_version $v
    d=/tmp/decoders-imagemagick-$v
    # Only these two, as on a computer without astropy and pydicom: fits.desktop and
    # dicom.desktop come first by name and would take the files otherwise.
    rm -rf $(formats_dir) && mkdir -p $d $(formats_dir)
    cp /t/new/examples/formats/*-imagemagick.desktop $(formats_dir)/
    # Dark at the top, light at the bottom, both.
    convert -size 600x400 gradient:black-white $d/a.fits
    python3 - "$d" <<'PY'
import sys, numpy
from pydicom.dataset import Dataset, FileMetaDataset
from pydicom.uid import ExplicitVRLittleEndian, generate_uid, SecondaryCaptureImageStorage
meta = FileMetaDataset()
meta.MediaStorageSOPClassUID = SecondaryCaptureImageStorage
meta.MediaStorageSOPInstanceUID = generate_uid()
meta.TransferSyntaxUID = ExplicitVRLittleEndian
ds = Dataset()
ds.file_meta = meta
ds.SOPClassUID, ds.SOPInstanceUID = meta.MediaStorageSOPClassUID, meta.MediaStorageSOPInstanceUID
ds.Modality, ds.Rows, ds.Columns = "OT", 400, 600
ds.SamplesPerPixel, ds.PhotometricInterpretation = 1, "MONOCHROME2"
ds.BitsAllocated, ds.BitsStored, ds.HighBit, ds.PixelRepresentation = 8, 8, 7, 0
ds.PixelData = numpy.fromfunction(lambda r, c: r * 255 / 399, (400, 600)).astype(numpy.uint8).tobytes()
ds.is_little_endian, ds.is_implicit_VR = True, False
ds.save_as(f"{sys.argv[1]}/b.dcm", write_like_original=False)
PY
    config
    start $d/a.fits
    sleep 3
    shot decoder-im-fits-$v
    key space; sleep 3
    shot decoder-im-dcm-$v
    quit
    dark_light() { [ $(red $1 760 120) -lt 60 ] && [ $(red $1 760 700) -gt 190 ] && echo yes; }
    if [ $v = new ]; then
        expect "a .fits through ImageMagick" "$(dark_light decoder-im-fits-new)" yes
        expect "a .dcm through ImageMagick" "$(dark_light decoder-im-dcm-new)" yes
    else note "old: fits $(dark_light decoder-im-fits-old), dcm $(dark_light decoder-im-dcm-old)"; fi
done

section "science formats: a NIfTI volume, an HDF5 dataset and a pathology slide shown"
for v in new old; do
    as_version $v
    d=/tmp/science-$v
    mkdir -p $d $(formats_dir)
    cp /t/new/examples/formats/* $(formats_dir)/
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

section "a NIfTI series in FSL's orientation (LAS, 4D): its first volume, the front at the top"
d=/tmp/nifti-series
mkdir -p $d $(formats_dir)
cp /t/new/examples/formats/* $(formats_dir)/
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

section "the Bio-Formats format: a microscope file (ICS) shown through bftools"
for v in new old; do
    as_version $v
    d=/tmp/bioformats-$v
    mkdir -p $d $(formats_dir)
    cp /t/new/examples/formats/* $(formats_dir)/
    # ICS, which the example leaves out for calendar invitations, added as its comment says.
    sed -i 's/^Extensions=czi;/Extensions=ics;ids;czi;/' $(formats_dir)/bioformats.desktop
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

section "the MRC format: a cryo-EM map's middle section, its top at the top"
for v in new old; do
    as_version $v
    d=/tmp/mrc-$v
    mkdir -p $d $(formats_dir)
    cp /t/new/examples/formats/* $(formats_dir)/
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

section "the DICOM format: an X-ray in the window it names, MONOCHROME1 the right way"
for v in new old; do
    as_version $v
    d=/tmp/dicom-$v
    mkdir -p $d $(formats_dir)
    cp /t/new/examples/formats/* $(formats_dir)/
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

section "the ImageMagick formats: a Photoshop file and a TGA shown, a PNG named .tga not"
for v in new old; do
    as_version $v
    d=/tmp/graphics-$v
    mkdir -p $d $(formats_dir)
    # Each version's own formats (examples/decoders before 0.75.0): the new ones name the
    # format for ImageMagick.
    cp $(ls -d /t/$v/examples/formats /t/$v/examples/decoders 2>/dev/null | head -1)/* $(formats_dir)/
    convert -size 600x400 xc:lime $d/a.psd
    convert -size 600x400 xc:blue $d/b.tga
    # Another format under an added format's extension, as a file from the internet could be.
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

section "real files of each format (samples.txt): RAW of eight makes, science and graphics files"
for v in new old; do
    as_version $v
    mkdir -p $(formats_dir)
    cp /t/new/examples/formats/* $(formats_dir)/
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

section "a long error under the image: broken into lines inside the image area"
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
    # The image area starts at x=240. Its first 10 pixels have no text, while the place where
    # the error's lines start has some, so the error was drawn.
    edge=$(convert /t/out/long-error-$v.png -crop 10x400+241+200 -format "%[fx:int(255*maxima.r)]" info:)
    lines=$(convert /t/out/long-error-$v.png -crop 100x100+270+370 -format "%[fx:int(255*maxima.r)]" info:)
    if [ $v = new ]; then expect "the error drawn where its lines start" "$([ $lines -ge 100 ] && echo text)" text
        expect "nothing at the image area's edge" "$([ $edge -lt 40 ] && echo none)" none
    else note "old, at the edge: $edge"; fi
done

section "search terms combined: and, not (-), or (|) and groups ( )"
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
    # The matches of a search, read back: all of them selected, then asked for. FIRST is the
    # first match's name, clicked to put the keys in the list, where Ctrl+A selects every match.
    found() {
        click 120 284; key ctrl+a; xdotool type "$1"; sleep 2
        menu "$2"; key ctrl+a; sleep 1
        remote selection | tr '\0' '\n' | xargs -n1 basename 2>/dev/null | tr '\n' ' '
    }
    and_not=$(found 'sunset -re:png$' a_sunset.jpg)
    either=$(found 'beach | re:png$' b_sunset.png)
    grouped=$(found '( sunset | beach ) -( re:png$ )' a_sunset.jpg)
    quit
    if [ $v = new ]; then
        expect "sunset and not .png" "$and_not" "a_sunset.jpg "
        expect "beach, or .png" "$either" "b_sunset.png c_beach.jpg d_beach.png "
        expect "a group, and not a group" "$grouped" "a_sunset.jpg c_beach.jpg "
    else note "old: '$and_not' '$either' '$grouped'"; fi
done

section "the .* button beside the search: regular expressions, saved, and the panel stays put"
for v in new old; do
    as_version $v
    d=/tmp/search-mode-$v
    mkdir -p $d
    for f in a.jpg b.jpg c.jpg; do convert -size 300x200 xc:red $d/$f; done
    config
    start $d
    sleep 2
    # The panel's edge and the image's top beside it, before and after: a row wider than the
    # panel widened it every frame, which moved both.
    shot search-mode-$v-0
    menu ".*"; sleep 3
    shot search-mode-$v-1
    view() { convert /t/out/search-mode-$v-$1.png -crop 1280x60+0+60 +repage rgb:- | md5sum; }
    # Both screenshots taken: of two missing ones the empty strips would be the same too.
    same=$([ -s /t/out/search-mode-$v-0.png ] && [ -s /t/out/search-mode-$v-1.png ] \
        && [ "$(view 0)" = "$(view 1)" ] && echo yes)
    mode=$(grep -o '^fuzzy_search=.*' $XDG_CONFIG_HOME/glimt/config)
    # A pattern fuzzy search finds nothing with: a class of two letters at the start.
    click 120 284; key ctrl+a; xdotool type '^[bc]'; sleep 2
    menu b.jpg; key ctrl+a; sleep 1
    found=$(remote selection | tr '\0' '\n' | xargs -n1 basename 2>/dev/null | tr '\n' ' ')
    menu ".*"; sleep 1
    back=$(grep -o '^fuzzy_search=.*' $XDG_CONFIG_HOME/glimt/config)
    quit
    if [ $v = new ]; then
        expect "the button turns regular expressions on, saved" "$mode" fuzzy_search=false
        expect "^[bc] finds b and c" "$found" "b.jpg c.jpg "
        expect "the image beside the list stays where it was" "$same" yes
        expect "pressed again: fuzzy search" "$back" fuzzy_search=true
    else note "old: '$mode' '$found' '$same' '$back'"; fi
done

section "a slideshow from a plain list: its order, relative paths, and the list never written"
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
    first=$(remote current | tr '\0' '\n' | xargs -n1 basename 2>/dev/null)
    key space; sleep 1
    second=$(remote current | tr '\0' '\n' | xargs -n1 basename 2>/dev/null)
    # Delete in a list: out of the list in the window only.
    key Delete; sleep 1
    quit
    left="$(ls $d/pics | tr '\n' ' ')$([ "$(md5sum <$d/show.txt)" = "$before" ] && echo same)"
    if [ $v = new ]; then
        expect "the list's order" "$first $second" "c.jpg a.jpg"
        expect "the images and the list untouched" "$left" "a.jpg b.jpg c.jpg same"
    else note "old: '$first' '$second' '$left'"; fi
done

section "the desktop asks for high contrast: Glimt takes its High Contrast theme, unless turned off"
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
as_version new
# The empty part of the file list: black in High Contrast, grey in the Dark theme.
expect "High Contrast while the desktop asks for it" "$(red contrast-new-true 120 700)" 0
differ "the chosen theme when that is turned off" "$(red contrast-new-false 120 700)" 0
note "old: $(red contrast-old-true 120 700)"

section "the size of menus and lists: a setting, and GNOME's text scaling followed"
d=/tmp/ui-scale
mkdir -p $d
convert -size 3000x2000 xc:red $d/a.jpg
# GNOME's 1.5 as the settings portal says it (gdbus), and as gsettings did before 0.50.0.
cat >/tmp/stub/gdbus <<'STUB'
#!/bin/sh
case "$*" in
*text-scaling-factor*) echo "(<1.5>,)" ;;
*) exit 1 ;;
esac
STUB
printf '#!/bin/sh\necho 1.5\n' >/tmp/stub/gsettings
chmod +x /tmp/stub/gdbus /tmp/stub/gsettings
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
as_version new
differ "at 200 %, the list where the image began" "$(colour scale-new-ui_scale=200-false 400 400)" red
differ "GNOME's text scaling followed" "$(colour scale-new-ui_scale=100-true 300 400)" red
expect "not followed when turned off" "$(colour scale-new-ui_scale=100-false 300 400)" red
note "old at 200 %: $(colour scale-old-ui_scale=200-false 400 400)"

section "Back from a folder opened in a collection: the collection again"
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
    remote open $d/y >/dev/null 2>&1; until_current $d/y/b.jpg
    click 22 35 2                             # Back, in the toolbar
    key End; sleep 1
    got=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    quit
    if [ $v = new ]; then expect "the collection's last image" "$got" "y/b.jpg"
    else note "old: $got"; fi
done

section "a collection put in order with keys: one place earlier, then to the end"
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

section "a collection put in order by dragging: a, the first row, dropped below c"
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

section "a collection put in order in the grid in the image area (G): c dropped before a"
for v in new old; do
    as_version $v
    d=/tmp/grid-drag-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n"%s/c.jpg"\n' $d $d $d >$d/show.gqv
    config
    start $d/show.gqv
    sleep 2
    key g; sleep 1
    # The grid is drawn after the side panel, so where names its cells.
    from=$(where c.jpg); to=$(where a.jpg)
    if [ -n "$from" ] && [ -n "$to" ]; then
        x0=${from% *}; y0=${from#* }; x1=$(( ${to% *} - 30 )); y1=${to#* }
        xdotool mousemove $x0 $y0; sleep 0.3; xdotool mousedown 1 2>/dev/null
        for i in 1 2 3 4 5 6 7 8; do
            xdotool mousemove $(( x0 + (x1 - x0) * i / 8 )) $(( y0 + (y1 - y0) * i / 8 ))
            sleep 0.05
        done
        sleep 0.5; xdotool mouseup 1 2>/dev/null; sleep 1
    fi
    quit
    order=$(grep -o '[abc]\.jpg' $d/show.gqv | tr '\n' ' ')
    if [ $v = new ]; then expect "the grid's drop, saved" "$order" "c.jpg a.jpg b.jpg "
    else note "old: $order (no grid)"; fi
done

section "a collection put in order in the side panel while the grid shows (G): a dropped below c"
for v in new old; do
    as_version $v
    d=/tmp/side-drag-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n"%s/c.jpg"\n' $d $d $d >$d/show.gqv
    # Names without thumbnails: the side panel's rows are where they are with the grid on.
    config thumbnails=false
    start $d/show.gqv
    sleep 2
    from=$(where a.jpg); to=$(where c.jpg)
    key g; sleep 1
    if [ -n "$from" ] && [ -n "$to" ]; then
        x=${from% *}; y0=${from#* }; y1=$(( ${to#* } + 6 ))
        xdotool mousemove $x $y0; sleep 0.3; xdotool mousedown 1 2>/dev/null
        for i in 1 2 3 4 5 6 7 8; do xdotool mousemove $x $(( y0 + (y1 - y0) * i / 8 )); sleep 0.05; done
        sleep 0.5; xdotool mouseup 1 2>/dev/null; sleep 1
    fi
    quit
    order=$(grep -o '[abc]\.jpg' $d/show.gqv | tr '\n' ' ')
    if [ $v = new ]; then expect "the side panel's drop, saved" "$order" "b.jpg c.jpg a.jpg "
    else note "old: $order"; fi
done

section "a drag in the side panel let go on the grid's empty space (G): the order stays"
for v in new old; do
    as_version $v
    d=/tmp/grid-away-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n"%s/c.jpg"\n' $d $d $d >$d/show.gqv
    # Names without thumbnails: the side panel's rows are where they are with the grid on.
    config thumbnails=false
    start $d/show.gqv
    sleep 2
    from=$(where a.jpg); to=$(where c.jpg)
    key g; sleep 1
    cell=$(where a.jpg)
    if [ -n "$from" ] && [ -n "$to" ] && [ -n "$cell" ]; then
        x=${from% *}; y0=${from#* }; y1=$(( ${to#* } + 6 ))
        xdotool mousemove $x $y0; sleep 0.3; xdotool mousedown 1 2>/dev/null
        for i in 1 2 3 4 5 6 7 8; do xdotool mousemove $x $(( y0 + (y1 - y0) * i / 8 )); sleep 0.05; done
        sleep 0.5
        # The line is below c; then over the grid below its only row, let go in the same move.
        xdotool mousemove ${cell% *} $(( ${cell#* } + 250 )) mouseup 1 2>/dev/null; sleep 1
    elif [ $v = new ]; then fail "the side panel's rows and the grid's cell are found"
    fi
    quit
    order=$(grep -o '[abc]\.jpg' $d/show.gqv | tr '\n' ' ')
    if [ $v = new ]; then expect "a drag let go off every row ends" "$order" "a.jpg b.jpg c.jpg "
    else note "old: $order (no grid)"; fi
done

section "a notice in the grid in the image area (G): shown over the thumbnails"
for v in new old; do
    as_version $v
    d=/tmp/grid-notice-$v
    mkdir -p $d
    for c in a b c; do convert -size 300x200 xc:red $d/$c.jpg; done
    printf '#GQview collection\n"%s/a.jpg"\n"%s/b.jpg"\n"%s/c.jpg"\n' $d $d $d >$d/show.gqv
    config
    start $d/show.gqv
    key g
    settled grid-notice-before-$v
    key ctrl+f; sleep 1                    # New Folder in a collection: a notice, no dialog
    shot grid-notice-after-$v
    quit
    changed=$(compare -metric AE /t/out/grid-notice-before-$v.png \
        /t/out/grid-notice-after-$v.png null: 2>&1 | cut -d' ' -f1)
    if [ $v = new ]; then
        if [ "${changed%.*}" -gt 500 ] 2>/dev/null; then pass "the notice shows ($changed pixels)"
        else fail "the notice shows: $changed pixels changed"; fi
    else note "old: $changed pixels changed"; fi
done

section "a rectangle dragged while the folder changes: the selection keeps its files"
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
    got=$(remote selection 2>/dev/null | tr '\0' ' ' | sed "s|$d/||g")
    quit
    if [ $v = new ]; then expect "b, c and d still selected, not a" "$got" "b.png c.png d.png "
    else note "old: $got"; fi
done

section "a rectangle held at the grid's bottom edge: the grid scrolls, the rows passed stay selected"
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
    n=$(remote selection 2>/dev/null | tr -cd '\0' | wc -c)
    quit
    if [ $v = new ]; then expect "more than the rows in view selected" "$([ $n -ge 20 ] && echo yes)" yes; fi
    note "$v: $n selected"
done

section "an empty collection: the keys that move images in it leave Glimt running"
for v in new old; do
    as_version $v
    d=/tmp/empty-collection-$v
    mkdir -p $d
    printf '#GQview collection\n' >$d/empty.gqv
    config
    start $d/empty.gqv
    sleep 2
    arrived=$(keys_arrive)
    key ctrl+shift+Up; key ctrl+shift+End; sleep 1
    running=$(kill -0 $GPID 2>/dev/null && echo running || echo crashed)
    quit
    if [ $v = new ]; then
        expect "the keys reach the window (the control)" "$arrived" yes
        expect "Glimt still runs" "$running" running
    else note "old: $running"; fi
done

section "a collection played in random order: an image taken out of it keeps the others' order"
for v in new old; do
    as_version $v
    d=/tmp/shuffled-$v
    mkdir -p $d
    for c in a b c d e f g h; do convert -size 300x200 xc:red $d/$c.jpg; done
    printf '#GQview collection\n' >$d/show.gqv
    for c in a b c d e f g h; do printf '"%s/%s.jpg"\n' $d $c >>$d/show.gqv; done
    config slideshow_random=true
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

section "Check Images: a photo cut off in copying and a file that is no picture found and marked"
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
    menu File "Check Images…" Check; sleep 4
    shot check-window-$v
    key Escape; sleep 1
    shot check-list-$v
    quit
    # The warning sign at the right end of a row of the list: orange (red 240), or the row's own
    # dark grey or selection blue.
    marked() { [ $(red check-list-$v 214 $1) -gt 200 ] && echo marked || echo -; }
    got="$(marked 346) $(marked 428) $(marked 510)"
    if [ $v = new ]; then expect "the cut and the false one marked, the whole one not" "$got" "- marked marked"
    else note "old: $got"; fi
    # The same marks in the grid of thumbnails (Ctrl+I), on each thumbnail's top left.
    if [ $v = new ]; then
        start $d; sleep 2
        menu File "Check Images…" Check; sleep 4; key Escape; sleep 1
        key ctrl+i; sleep 2
        shot check-grid-$v
        quit
        # Cells of 112: a_whole and b_cut in the first row, c_text below.
        # Orange (240 140 0), told from a red thumbnail by its green.
        cell() {
            set -- $(pixel check-grid-$v $1 $2)
            [ $1 -gt 200 ] && [ $2 -gt 100 ] && [ $2 -lt 180 ] && echo marked || echo -
        }
        expect "the same marks in the grid" "$(cell 19 322) $(cell 131 322) $(cell 19 426)" "- marked marked"
    fi
done

section "Check Images still running when another folder opens: nothing marked there"
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
    menu File "Check Images…" Check; sleep 2
    # The check runs: a's first files, cut, are marked before b opens.
    shot check-switch-a-$v
    remote open $d/b >/dev/null 2>&1
    sleep 15
    shot check-switch-$v
    quit
    marked() { [ $(red $1 214 $2) -gt 200 ] && echo marked || echo -; }
    before="$(marked check-switch-a-$v 510) $(marked check-switch-a-$v 592)"   # below its window
    got="$(marked check-switch-$v 346) $(marked check-switch-$v 428) $(marked check-switch-$v 510)"
    if [ $v = new ]; then
        expect "Check Images marks the cut files of the folder it began in" "$before" "marked marked"
        expect "the new folder's files not marked" "$got" "- - -"
    else note "old: $before, then $got"; fi
done

section "a settings file with a byte that is not UTF-8: a menu toggle saves, and leaves the rest as it was"
d=/tmp/config-latin1
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
for v in new old; do
    as_version $v
    printf 'slideshow_delay=7\nvideo_player=/opt/caf\351/player\n' \
        | sed "$([ $v = old ] && echo "$OLD_KEYS")" >$XDG_CONFIG_HOME/glimt/config
    start $d/a.jpg
    sleep 2
    key t; key t          # Thumbnails off and on: each saves the settings
    sleep 1
    quit
    # The toggles' save adds the keys the file lacked (thumbnails=): none when it was refused.
    got="$(grep -c 'delay=7$' $XDG_CONFIG_HOME/glimt/config) $(grep -c '^thumbnails=' $XDG_CONFIG_HOME/glimt/config)"
    if [ $v = new ]; then expect "saved, the own delay kept" "$got" "1 1"
    else note "old: delay=7 lines, thumbnails lines: $got"; fi
done

section "Select by mark: the files with mark 1 selected"
d=/tmp/select-mark
mkdir -p $d
for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
config
start $d/a.png
sleep 2
key space; key 1; key space; key 1          # mark 1 on b and c
key ctrl+1; sleep 1                          # Select by Mark 1
got=$(remote selection 2>/dev/null | tr '\0' ' ' | sed "s|$d/||g")
quit
expect "b and c selected" "$got" "b.png c.png "

section "Back Up Glimt's Data: settings, marks and collections copied to a dated folder"
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
    menu File "Back Up Glimt's Data…"; sleep 3
    quit
    b=$(ls -d $d/to/glimt-backup-* 2>/dev/null | head -1)
    got="$(cat "$b/config/config" 2>/dev/null | grep -c theme=Light) $(ls "$b/data/collections" 2>/dev/null) $(cut -f2 "$b/state/marks" 2>/dev/null)"
    if [ $v = new ]; then expect "the settings, the collection and the stars in the backup" "$got" "1 Mine.gqv 4"
    else note "old: $got"; fi
done

section "glimt --back-up FOLDER: the same backup without a window, a second one beside the first"
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

section "glimt --back-up: a folder that is not there is refused, a running rename's plan left out"
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
    if [ $v = new ]; then expect "refused, nothing made; no plan in the backup" "$got" "1 0 0"
        expect "a backup into Glimt's own folder refused" "$inside" 1
        # The control: the backup that left the plan out was made, with the collection in it.
        expect "the backup made (the control)" \
            "$(ls $d/to/glimt-backup-*/data/collections 2>/dev/null)" Mine.gqv
    else note "old: $got"; fi
done

section "Faster without a lock: the delay saved (so the two checks below test something)"
d=/tmp/faster-saved
mkdir -p $d
for f in a b; do convert -size 64x64 xc:red $d/$f.png; done
config slideshow_delay=5
start $d
sleep 2
key ctrl+equal; sleep 1
quit
expect "the delay changed" "$(grep -c 'delay=5$' $XDG_CONFIG_HOME/glimt/config)" 0

section "a kiosk: a visitor's keys change no saved setting"
for v in new old; do
    as_version $v
    d=/tmp/kiosk-keys-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    config slideshow_delay=5
    start --kiosk $d
    sleep 2
    # The control: End, which a kiosk allows, reaches it. A settings file the keys never
    # reached would also be as it was.
    key End; arrived=$(remote current 2>/dev/null | tr -d '\0')
    key ctrl+equal; key r; key i; sleep 1    # faster, random order, overlay
    timeout 30 $BIN --remote-all quit >/dev/null 2>&1; sleep 1
    quit
    got=$(grep -c 'delay=5$' $XDG_CONFIG_HOME/glimt/config)
    if [ $v = new ]; then expect "a key reaches the kiosk (the control)" "$arrived" $d/c.png
        expect "the delay as it was" "$got" 1
    else note "old: $(grep -E '^(delay|random|overlay)=' $XDG_CONFIG_HOME/glimt/config | tr '\n' ' ')"; fi
done

section "a kiosk without a runtime folder: told, and Ctrl+Q is its way out; with one, Ctrl+Q is refused"
d=/tmp/kiosk-way-out
mkdir -p $d
convert -size 64x64 xc:red $d/a.png
config
runtime=$XDG_RUNTIME_DIR
unset XDG_RUNTIME_DIR
start --kiosk $d
sleep 2
key ctrl+q; sleep 2
ended=$(kill -0 $GPID 2>/dev/null && echo running || echo ended)
told=$(grep -c 'XDG_RUNTIME_DIR' /tmp/glimt.err)
quit
export XDG_RUNTIME_DIR=$runtime
expect "told, and ended by Ctrl+Q" "$told $ended" "1 ended"
# With remote control, the kiosk stays, and settings off do not turn it off.
config remote_control=false
start --kiosk $d
sleep 2
key ctrl+q; sleep 2
stays=$(kill -0 $GPID 2>/dev/null && echo running || echo ended)
timeout 30 $BIN --remote-all quit >/dev/null 2>&1; sleep 2
left=$(kill -0 $GPID 2>/dev/null && echo running || echo ended)
quit
expect "Ctrl+Q refused, --remote quit taken (the control)" "$stays $left" "running ended"

section "Edit → Tools from the menu: the tool runs on the shown file, Glimt answering meanwhile"
d=/tmp/tool-menu
mkdir -p $d $XDG_DATA_HOME/glimt/tools
convert -size 64x64 xc:red $d/a.png
# The tool stays, as an editor would.
printf '#!/bin/sh\necho "$@" >>%s/ran\nsleep 20\n' $d >/tmp/stub/stamp; chmod +x /tmp/stub/stamp
printf '[Desktop Entry]\nName=Stamp\nExec=stamp %%f\n' >$XDG_DATA_HOME/glimt/tools/stamp.desktop
config
start $d/a.png; sleep 2
menu Edit Tools; sleep 1
shot tools-menu
menu Stamp; sleep 2
ran=$(cat $d/ran 2>/dev/null)
answer=$(remote current 2>/dev/null | tr -d '\0')
# Still running, it has Stop in the menu, which ends it with its sleep.
menu Edit Tools "Stop Stamp"
until_ok "! pgrep -f /tmp/stub/stamp" 5
left=$(pgrep -f /tmp/stub/stamp >/dev/null && echo running || echo ended)
quit
expect "the tool ran on a.png, and --remote current answered meanwhile" "$ran $answer" "$d/a.png $d/a.png"
expect "Stop Stamp in Edit → Tools ended the tool" "$left" "ended"

# In the folder's old name, plugins, which every version reads: NEW still reads it.
section "a tool with MimeType= in the old plugins folder: given only the selected files of its types"
for v in new old; do
    as_version $v
    d=/tmp/tool-types-$v
    mkdir -p $d $XDG_DATA_HOME/glimt/plugins
    convert -size 64x64 xc:red $d/a.png
    convert -size 64x64 xc:red $d/b.jpg
    printf '#!/bin/sh\necho "$@" >>%s/ran\n' $d >/tmp/stub/pngonly; chmod +x /tmp/stub/pngonly
    printf '[Desktop Entry]\nName=PngOnly\nExec=pngonly %%F\nMimeType=image/png;\n' \
        >$XDG_DATA_HOME/glimt/plugins/pngonly.desktop
    config
    start $d/a.png; sleep 2
    remote select $d/a.png $d/b.jpg >/dev/null 2>&1
    # The menu was Plugins before 0.75.0.
    menu Edit $([ $v = old ] && echo Plugins || echo Tools) PngOnly; sleep 2
    quit
    rm -f $XDG_DATA_HOME/glimt/plugins/pngonly.desktop
    got=$(sed "s|$d/||g" $d/ran 2>/dev/null)
    if [ $v = new ]; then expect "the tool in plugins/ ran on a.png only" "$got" "a.png"
    else note "old: $got"; fi
done

section "zoom and fit keys, the wheel and a drag: 1:1 (z), fit (x), the width (h), the height (w), + and -"
d=/tmp/zoom-keys
mkdir -p $d
convert -size 400x300 xc:blue -fill red -draw 'rectangle 100,75 300,225' $d/a.png
config wheel_zoom=true
start $d/a.png; sleep 2
# Fitted, the image is 964 wide from x=278, its red middle from 519. Probes at y=410: x=260
# (left of the fitted image), 470 (blue when fitted, and with the width filled) and 600 (red
# when fitted).
probe() { shot $1; echo "$(colour $1 260 410) $(colour $1 470 410) $(colour $1 600 410)"; }
key z; z=$(probe zoom-z)                       # 400x300 at the centre: 560-960, red 660-860
key x; x=$(probe zoom-x)
key h; h=$(probe zoom-h)                       # 1040 wide from 240, red from 520
key w; w=$(probe zoom-w)                       # the height: as fitted
key equal; key equal; in=$(probe zoom-in)      # 1.56 times: red from 383
key minus; key minus; out=$(probe zoom-out)    # fitted again
key equal; key equal
xdotool mousemove 900 410 mousedown 1; sleep 0.3; xdotool mousemove 950 410; sleep 0.3
xdotool mousemove 1050 410; sleep 0.3; xdotool mouseup 1; sleep 1
drag=$(probe zoom-drag)                        # 150 to the right: red from 533
key x; xdotool mousemove 760 410 click 4 click 4; sleep 1
wheel=$(probe zoom-wheel)                      # two notches in at the centre: about 1.6 times
quit
expect "z: the image at its size" "$z" "other other blue"
expect "x: fitted" "$x" "other blue red"
expect "h: the width filled" "$h" "blue blue red"
expect "w: the height, as fitted" "$w" "other blue red"
expect "+ twice: zoomed in" "$in" "blue red red"
expect "- twice: fitted again" "$out" "other blue red"
expect "dragged to the right: blue where red was" "$drag" "blue blue red"
expect "the wheel zooms in" "$wheel" "blue red red"

section "several folders on the command line: one list, each image with its folder"
d=/tmp/two-folders
mkdir -p $d/one $d/two
convert -size 64x64 xc:red $d/one/a.png
convert -size 64x64 xc:blue $d/two/b.png
config
start $d/one $d/two; sleep 2
key ctrl+a
listed=$(remote selection 2>/dev/null | tr '\0' '\n' | sed "s|$d/||" | sort | tr '\n' ' ')
quit
expect "both folders' images in one list" "$listed" "one/a.png two/b.png "

section "Open Folder Recursive: the folder with its subfolders, and Include Subfolders on"
d=/tmp/open-recursive
mkdir -p $d/tree/sub/deep $d/start
convert -size 64x64 xc:red $d/tree/a.png
convert -size 64x64 xc:blue $d/tree/sub/b.png
convert -size 64x64 xc:lime $d/tree/sub/deep/c.png
convert -size 64x64 xc:yellow $d/start/s.png
answer $d/tree
config
start $d/start; sleep 2
menu File "Open Folder Recursive…"; sleep 3
key ctrl+a
listed=$(remote selection 2>/dev/null | tr '\0' '\n' | sed "s|$d/tree/||" | sort | tr '\n' ' ')
quit
expect "the tree's three images" "$listed" "a.png sub/b.png sub/deep/c.png "
expect "Include Subfolders on in the settings" "$(grep -c '^show_subfolders=true$' $XDG_CONFIG_HOME/glimt/config)" 1

section "New Folder (Ctrl+F): made in the folder shown"
d=/tmp/new-folder
mkdir -p $d
convert -size 64x64 xc:red $d/a.png
config
start $d; sleep 2
key ctrl+f; sleep 1; xdotool type holiday; key Return; sleep 1
quit
expect "the folder is there" "$([ -d $d/holiday ] && echo yes)" yes

section "a settings file of 0.79: icons=true shows the icon grid, as icon_grid=true does"
d=/tmp/old-setting
mkdir -p $d
for c in red lime blue; do convert -size 64x64 xc:$c $d/$c.png; done
for setting in "" icons=true icon_grid=true; do
    config $setting
    start $d/red.png; sleep 2
    settled old-setting-${setting%%=*}
    quit
done
same() { md5sum </t/out/$1.png; }
differ "icon_grid=true shows another list" "$(same old-setting-icon_grid)" "$(same old-setting-)"
expect "the old key icons=true as icon_grid=true" "$(same old-setting-icons)" "$(same old-setting-icon_grid)"

section "the action names of 0.79: Up in keys= and --remote run Home still work"
d=/tmp/old-actions
mkdir -p $d/sub
convert -size 64x64 xc:red $d/sub/a.png
convert -size 64x64 xc:blue $d/b.png
convert -size 64x64 xc:lime $HOME/h.png
config "keys=Up=F9"
start $d/sub/a.png; sleep 2
key F9; sleep 2
up=$(remote current 2>/dev/null | tr -d '\0')
remote run Home >/dev/null 2>&1; ran=$?
sleep 2
home=$(remote current 2>/dev/null | tr -d '\0')
quit
expect "F9 for Up goes to the parent, run Home to the home folder" "$up $ran $home" "$d/b.png 0 $HOME/h.png"

section "Up and Home in the toolbar: the parent folder, then the home folder"
d=/tmp/up-home
mkdir -p $d/sub
convert -size 64x64 xc:red $d/sub/a.png
convert -size 64x64 xc:blue $d/b.png
convert -size 64x64 xc:lime $HOME/h.png
config
start $d/sub/a.png; sleep 2
click 99 35 2                                          # Up
up=$(remote current 2>/dev/null | tr -d '\0')
click 137 35 2                                         # Home
home=$(remote current 2>/dev/null | tr -d '\0')
quit
expect "Up shows the parent's image, Home the home folder's" "$up $home" "$d/b.png $HOME/h.png"

section "Mirror, Flip and Rotate 180 as shown: the quarters of the image change places"
d=/tmp/turns-shown
mkdir -p $d
convert -size 200x150 xc:red -size 200x150 xc:blue +append \
    \( -size 200x150 xc:lime -size 200x150 xc:yellow +append \) -append $d/a.png
config
start $d/a.png; sleep 2
# Fitted, the image spans x 278-1242 and y 50-772: the top left and bottom right quarters.
corners() { shot $1; echo "$(colour $1 400 150) $(colour $1 1100 650)"; }
plain=$(corners turns-plain)
key shift+m; mirrored=$(corners turns-mirror); key shift+m
key shift+f; flipped=$(corners turns-flip); key shift+f
key shift+r; turned=$(corners turns-180)
quit
expect "as it is: red top left, yellow bottom right" "$plain" "red yellow"
expect "Mirror: blue top left, green bottom right" "$mirrored" "blue green"
expect "Flip: green top left, blue bottom right" "$flipped" "green blue"
expect "Rotate 180: yellow top left, red bottom right" "$turned" "yellow red"

section "Play Video (Enter) runs the player set in Preferences with the file"
d=/tmp/play-video
mkdir -p $d
ffmpeg -v error -f lavfi -i color=c=blue:s=320x240:d=0.2 -pix_fmt yuv420p $d/v.mp4
printf '#!/bin/sh\necho "$@" >>%s/ran\n' $d >/tmp/stub/player; chmod +x /tmp/stub/player
config video_player=/tmp/stub/player
start $d/v.mp4; sleep 2
key Return; sleep 2
quit
expect "the player got the video" "$(cat $d/ran 2>/dev/null)" "$d/v.mp4"

section "Save This Frame and Export Frames of an animation, from the File menu"
d=/tmp/frames
mkdir -p $d/out
convert -size 400x300 xc:red $d/a.png
convert -size 400x300 xc:blue $d/b.png
convert -delay 50 $d/a.png $d/b.png $d/anim.gif
config
start $d/anim.gif; sleep 2
answer $d/frame.png
menu File "Save This Frame…"; sleep 3
answer $d/out
menu File "Export Frames…"; sleep 3
quit
expect "the frame saved as PNG, 400x300" "$(identify -format '%wx%h' $d/frame.png 2>/dev/null)" 400x300
expect "both frames exported into a new folder" "$(find $d/out -name '*.png' | wc -l | tr -d ' ')" 2

section "the histogram (K, J) draws over the image, and goes away again"
d=/tmp/histogram-keys
mkdir -p $d
convert -size 400x300 gradient:red-blue $d/a.png
config
start $d/a.png; sleep 2
area() { shot $1; convert /t/out/$1.png -crop 1000x700+260+60 +repage rgb:- | md5sum; }
base=$(area hist-off); key k; rgb=$(area hist-rgb); key j; log=$(area hist-log)
key k; bright=$(area hist-bright); key k; off=$(area hist-off2)
quit
differ "K: the RGB histogram shows" "$base" "$rgb"
differ "J: the logarithmic one differs" "$rgb" "$log"
differ "K: brightness differs from RGB" "$log" "$bright"
expect "K: off again, the image as before" "$off" "$base"

section "full screen info (I): shown in full screen, and hidden by I"
d=/tmp/overlay-key
mkdir -p $d
convert -size 400x300 xc:blue $d/a.png
config
start $d/a.png; sleep 2
key f; sleep 2
corner() { shot $1; convert /t/out/$1.png -crop 500x240+0+0 +repage rgb:- | md5sum; }
on=$(corner osd-on); key i; off=$(corner osd-off); key i; again=$(corner osd-on2)
key Escape
quit
differ "I hides the info" "$on" "$off"
expect "I shows it again" "$again" "$on"

section "Compare (E) and the layouts (L, U, Y, Alt+arrow): the next image beside, below, in a pane"
d=/tmp/layout-keys
mkdir -p $d
convert -size 400x300 xc:red $d/a.png
convert -size 400x300 xc:blue $d/b.png
config
start $d/a.png; sleep 2
# Probes: the left and right halves (y=300), and the top and bottom halves (x=700), where a
# pane's fitted image lies in each layout; a pane edge is never probed.
probe() { shot $1; echo "$(colour $1 500 300) $(colour $1 1020 300) $(colour $1 700 230) $(colour $1 700 590)"; }
key e; sleep 1; compare=$(probe compare-on); key e; sleep 1; alone=$(probe compare-off)
key l; sleep 1; two=$(probe layout-2x1)
key l; sleep 1; above=$(probe layout-1x2)
key l; sleep 1; four=$(probe layout-2x2)
key y; sleep 1; one=$(probe layout-1)
key u; sleep 1; up=$(probe layout-u)
key l; key l; sleep 1                                   # 2x2, then back to 2x1
key alt+Right; sleep 1                                  # the right pane's image shown
pane=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
quit
expect "E: a left, b right" "$compare" "red blue red red"
expect "E again: a alone" "$alone" "red red red red"
expect "L: two side by side" "$two" "red blue red red"
expect "L: two above each other" "$above" "other other red blue"
expect "L: four, a top left and b top right" "$(echo $four | cut -d' ' -f1-2)" "red blue"
expect "Y: one image" "$one" "red red red red"
expect "U: two above each other" "$up" "other other red blue"
expect "Alt+Right shows the right pane's image" "$pane" "b.png"

section "Preferences → Keyboard shortcuts: Change gives the action the next key"
d=/tmp/key-capture
mkdir -p $d
convert -size 64x64 xc:red $d/a.png
config
start $d/a.png; sleep 2
key ctrl+o; sleep 2
xdotool mousemove 240 300 click 5 click 5 click 5; sleep 1   # down to the Keyboard section
menu "Keyboard shortcuts"; sleep 1
shot key-capture-table
menu "Change Auto Rotate Using EXIF"; key F9; sleep 1
key Escape; sleep 1
quit
expect "the own key in the settings" "$(grep -c 'AutoRotate=F9' $XDG_CONFIG_HOME/glimt/config)" 1

section "the Help windows: Help Manual (F1), Keyboard Shortcuts, Keyboard Map and About open"
d=/tmp/help-windows
mkdir -p $d
convert -size 64x64 xc:red $d/a.png
config
start $d/a.png; sleep 2
whole() { shot $1; md5sum </t/out/$1.png; }
base=$(whole help-none)
key F1; sleep 2; manual=$(whole help-manual); key Escape; sleep 1
menu Help "Keyboard Shortcuts"; sleep 2; keys=$(whole help-keys); key Escape; sleep 1
menu Help "Keyboard Map"; sleep 2; map=$(whole help-map); key Escape; sleep 1
menu Help "About Glimt"; sleep 2; about=$(whole help-about)
quit
differ "F1 opens the manual" "$base" "$manual"
differ "Keyboard Shortcuts opens" "$base" "$keys"
differ "Keyboard Map opens" "$base" "$map"
differ "About opens" "$base" "$about"
differ "the four are different windows" "$manual$keys" "$map$about"

section "Open With: the default application and one from the Open With list, both through gio"
d=/tmp/open-with
mkdir -p $d $XDG_DATA_HOME/applications
convert -size 64x64 xc:red $d/a.png
printf '[Desktop Entry]\nName=Stamp\nExec=true %%f\n' >$XDG_DATA_HOME/applications/stamp.desktop
# A stand-in gio: the type and its applications answered, open and launch written down.
cat >/tmp/stub/gio <<STUB
#!/bin/sh
case "\$1" in
  info) echo "  standard::content-type: image/png" ;;
  mime) printf 'Registered applications:\n\tstamp.desktop\n' ;;
  *) echo "\$*" >>$d/ran ;;
esac
STUB
chmod +x /tmp/stub/gio
config
start $d/a.png; sleep 2
menu File "Open With Default App"; sleep 2
menu File "Open With"; sleep 3                         # the list, from gio
shot open-with
menu Stamp; sleep 2
quit
expect "gio open, then gio launch of the chosen application" "$(cat $d/ran 2>/dev/null | tr '\n' ' ')" \
    "open $d/a.png launch $XDG_DATA_HOME/applications/stamp.desktop $d/a.png "

section "Copy Image copies the image as shown: turned by hand"
for v in new old; do
    as_version $v
    d=/tmp/clipboard-turned-$v
    mkdir -p $d
    convert -size 64x48 xc:red $d/a.png
    config
    start $d/a.png; sleep 2
    key bracketright; sleep 1        # turned a quarter clockwise, not saved
    if [ $v = new ]; then menu Edit "Copy Image to Clipboard"
    else menu Select "Copy Image to Clipboard"; fi   # 0.79 had it in the Select menu
    sleep 3
    size=$(xclip -o -selection clipboard -t image/png 2>/dev/null | identify -format '%wx%h' - 2>/dev/null)
    quit
    if [ $v = new ]; then expect "the turned image, 48x64" "$size" 48x64
    else note "old: $size"; fi
done

section "the clipboard: Copy Path and Copy Files from the Select menu, Copy Image from Edit, read back with xclip"
d=/tmp/clipboard
mkdir -p $d
convert -size 64x48 xc:red $d/a.png
config
start $d/a.png; sleep 2
menu Select "Copy Path to Clipboard"; sleep 1
path=$(xclip -o -selection clipboard 2>/dev/null)
menu Edit "Copy Image to Clipboard"; sleep 3
size=$(xclip -o -selection clipboard -t image/png 2>/dev/null | identify -format '%wx%h' - 2>/dev/null)
menu Select "Copy Files to Clipboard"; sleep 1
files=$(xclip -o -selection clipboard -t text/uri-list 2>/dev/null | tr -d '\r' | grep -c "file://$d/a.png")
quit
expect "the path, quoted" "$path" "\"$d/a.png\""
expect "the image, 64x48 as PNG" "$size" 64x48
expect "the file as a URI" "$files" 1

section "the interface in Norwegian in a real window: LANG changes the menu bar, the language setting rules"
d=/tmp/norsk
mkdir -p $d
convert -size 64x64 xc:red $d/a.png
bar() { convert /t/out/$1.png -crop 300x22+0+0 rgb:- | md5sum; }
config
start $d/a.png; sleep 2; shot lang-en; quit
export LANG=nb_NO.UTF-8
start $d/a.png; sleep 2; shot lang-nb; quit
config language=en
start $d/a.png; sleep 2; shot lang-en-set; quit
unset LANG
differ "LANG=nb_NO: another menu bar" "$(bar lang-en)" "$(bar lang-nb)"
expect "language=en: the English menu bar again" "$(bar lang-en-set)" "$(bar lang-en)"

section "--lock-settings, then New Window: the new window is locked too"
for v in new old; do
    as_version $v
    d=/tmp/lock-new-window-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    config slideshow_delay=5
    start --lock-settings $d
    sleep 2
    key ctrl+n; sleep 3                   # a new window: Faster pressed there
    other=$(xdotool search --name Glimt 2>/dev/null | grep -vx "$WIN" | tail -1)
    [ $v = new ] && expect "a second window opened" "$([ -n "$other" ] && echo yes)" yes
    # The control: End reaches the new window, whose title then names c.png, not the first one.
    xdotool windowfocus $other key End 2>/dev/null; sleep 1
    arrived="$(xdotool getwindowname $WIN 2>/dev/null) | $(xdotool getwindowname $other 2>/dev/null)"
    xdotool windowfocus $other key ctrl+equal 2>/dev/null; sleep 1
    timeout 30 $BIN --remote-all quit >/dev/null 2>&1; sleep 1
    quit
    got=$(grep -c 'delay=5$' $XDG_CONFIG_HOME/glimt/config)
    if [ $v = new ]; then expect "a key reaches the second window (the control)" "$arrived" "a.png - Glimt | c.png - Glimt"
        expect "the delay as it was" "$got" 1
    else note "old: $(grep '^delay=' $XDG_CONFIG_HOME/glimt/config)"; fi
done

section "the keyboard focus shown: Tab to a toolbar button, a ring in the accent colour"
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
    menu Up; sleep 1        # a click on a button: the ring goes
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

section "view filters: Shift+G shows a red image grey, then inverted, and the filter is kept"
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
    [ $v = new ] && kept=$(grep -c '^view_filter=inverted$' $XDG_CONFIG_HOME/glimt/config)
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

section "Tab to a menu, then a click on that same menu: the shortcuts work again"
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
    got=$(remote current 2>/dev/null | tr -d '\0' | sed "s|$d/||")
    quit
    if [ $v = new ]; then expect "End shows the last image" "$got" "c.png"
    else note "old: $got"; fi
done

section "the projector shows the frame of an animation stepped to"
for v in new old; do
    as_version $v
    d=/tmp/projector-frame-$v
    mkdir -p $d
    # Red for 20 s, then blue: stepped to, the blue frame is shown at once.
    convert -delay 2000 -size 300x200 xc:red xc:blue $d/a.gif
    config
    start $d/a.gif
    sleep 2
    menu Window Projector; sleep 1
    key ctrl+Next; sleep 1             # the next frame: blue
    p=$(xdotool search --name "Glimt Projector" 2>/dev/null | head -1)
    # `key` raised the main window over the projector, and the shot takes what is on top.
    [ -n "$p" ] && xdotool windowraise $p 2>/dev/null; sleep 1
    [ -n "$p" ] && import -window $p /t/out/projector-frame-$v.png
    quit
    if [ $v = new ]; then expect "the blue frame on the projector" "$(colour projector-frame-new 480 300)" blue
    else note "old: $(colour projector-frame-old 480 300 2>/dev/null)"; fi
done

section "a projector larger than the main view: the image sharp there"
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
    menu Window Projector; sleep 1
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

section "the keyword tree: a box sets the keyword, with its place in the tree"
d=/tmp/keywords
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
printf 'Animals\nPlaces|Norway|Oslo\n' >$XDG_CONFIG_HOME/glimt/keywords
config info_panel=true
start $d
sleep 2
# Frames drawn as the pointer moves: a panel that grows with each moves the boxes away.
# The tree's boxes are below Keywords, Comment, Alt text and Extended description.
for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
sleep 1
shot keywords-before
# The Animals box: on, then off again.
menu Animals; sleep 2
shot keywords-on
on=$(cat $d/a.jpg.xmp 2>/dev/null | grep -c '<rdf:li>Animals</rdf:li>')
menu Animals; sleep 2
off=$(cat $d/a.jpg.xmp 2>/dev/null | grep -c '<rdf:li>Animals</rdf:li>')
quit
expect "a box sets the keyword and its tree path" "$on" 2
expect "a box clears them again" "$off" 0

section "two windows writing one sidecar at once: each waits for the other, both changes kept"
for v in new old; do
    as_version $v
    d=/tmp/sidecar-two-$v
    mkdir -p $d
    convert -size 300x200 xc:red $d/a.jpg
    printf 'Animals\nPlaces|Norway|Oslo\n' >$XDG_CONFIG_HOME/glimt/keywords
    config info_panel=true xmp_stars=true
    # Every sidecar lock held from outside, as by another window in the middle of its write.
    rm -f /tmp/locks-held
    python3 -c "
import fcntl, os, time
d = os.environ['XDG_RUNTIME_DIR'] + '/glimt/sidecar-locks'
os.makedirs(d, mode=0o700, exist_ok=True)
held = [open(f'{d}/{i}', 'w') for i in range(64)]
for f in held:
    fcntl.flock(f, fcntl.LOCK_EX)
open('/tmp/locks-held', 'w').close()
time.sleep(20)" &
    locker=$!
    for i in $(seq 50); do [ -e /tmp/locks-held ] && break; sleep 0.1; done
    start $d/a.jpg
    first=$GPID first_win=$WIN
    sleep 2
    start $d/a.jpg                   # the second window, on top
    sleep 2
    for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
    sleep 1
    menu Animals; sleep 2            # the second window: the Animals keyword
    # The first window is raised before its key, as a window manager raises the window it
    # focuses. Xvfb has none, and eframe runs no frame for a covered window, so the key waited
    # unread. Since 0.76.0 the window takes --remote quit while covered, so the key was lost.
    xdotool windowraise $first_win
    WIN=$first_win; key alt+3; sleep 2   # the first window: three stars
    during=$(cat $d/a.jpg.xmp 2>/dev/null | wc -c)
    wait $locker
    sleep 3
    quit
    kill $first 2>/dev/null; wait $first 2>/dev/null
    stars=$(grep -c 'Rating="3"\|Rating>3<' $d/a.jpg.xmp 2>/dev/null)
    animals=$(grep -c '<rdf:li>Animals</rdf:li>' $d/a.jpg.xmp 2>/dev/null)
    if [ $v = new ]; then
        expect "nothing written while another holds the sidecar's lock" "$during" 0
        expect "the stars and the keyword both in the sidecar" "$stars $animals" "1 2"
    else note "old: $during bytes written meanwhile; stars ${stars:-0}, keyword ${animals:-0}"; fi
done

section "the keyword tree: removing an inner level takes the levels below it"
d=/tmp/keywords-remove
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
for v in new old; do
    as_version $v
    printf 'Animals\nPlaces|Norway|Oslo\n' >$XDG_CONFIG_HOME/glimt/keywords
    config info_panel=true
    start $d
    sleep 2
    for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
    sleep 1
    # Right-click Places, which holds only Norway|Oslo, and Remove from the tree in its menu.
    rclick Places; sleep 1
    shot keywords-remove-menu-$v
    menu "Remove from the tree"; sleep 2
    shot keywords-remove-$v
    quit
    left=$(tr '\n' ' ' <$XDG_CONFIG_HOME/glimt/keywords | sed 's/ $//')
    if [ $v = new ]; then expect "removing Places takes Norway and Oslo with it" "$left" Animals
    else note "old, left in the tree: $left"; fi
done

section "the keyword tree under --lock-settings: removed for the session, the file left as it was"
d=/tmp/keywords-locked
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
for v in new old; do
    as_version $v
    printf 'Animals\nPlaces|Norway|Oslo\n' >$XDG_CONFIG_HOME/glimt/keywords
    config info_panel=true
    start --lock-settings $d
    sleep 2
    for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
    sleep 1
    rclick Places; sleep 1
    menu "Remove from the tree"; sleep 2
    quit
    left=$(tr '\n' ' ' <$XDG_CONFIG_HOME/glimt/keywords | sed 's/ $//')
    if [ $v = new ]; then expect "the tree file as it was" "$left" "Animals Places|Norway|Oslo"
    else note "old, left in the tree: $left"; fi
done

section "a Lightroom catalog: its collection, keyword tree, stars and image keywords imported"
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
menu File "Import Lightroom Catalog…"; sleep 3
shot lightroom-dialog
menu Next; sleep 2
shot lightroom-plan
menu Import; sleep 3   # keeping Glimt's stars, with the images' keywords
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

section "a plain list (.m3u) is never renamed or trashed from the menu"
# Rename and Move Collection to Trash have no key, and Preferences offers none for them (they are
# not in SHORTCUTS): the menu is the only way to them.
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
    # Neither is in the File menu for a plain list. Open File is there, so the menu is open.
    menu File
    opened=$(where "Open File…" | grep -c .)
    absent="$(where "Move Collection to Trash")$(where "Rename Collection…")"
    key Escape
    quit
    got="$(cat $d/show.m3u 2>/dev/null | tr '\n' ' ')"
    if [ $v = new ]; then expect "the File menu open" "$opened" 1
        expect_empty "neither item in the menu" "$absent"
        expect "the list as it was" "$got" "#EXTM3U a.jpg "
    else note "old: '$got'"; fi
done

section "an image of a collection moved to another folder: the collection follows it"
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
    key ctrl+m; until_file $d/to/a.jpg; sleep 1    # a, shown, to the folder "to"
    quit
    got="$(grep -o '[a-z/-]*/[ab]\.jpg' $d/show.gqv | sed "s|$d/||" | tr '\n' ' ') $(ls $d/to)"
    if [ $v = new ]; then expect "the collection names a where it went" "$got" "to/a.jpg b.jpg  a.jpg"
    else note "old: $got"; fi
done

section "a move refused where the sidecar's name is another file's, and undone where the source stays"
for v in new old; do
    as_version $v
    d=/tmp/move-safe-$v
    mkdir -p $d/src $d/to $d/ro $d/to2
    convert -size 64x64 xc:red $d/src/a.jpg
    echo mine >$d/src/a.jpg.xmp
    echo orphan >$d/to/a.jpg.xmp
    # c.jpg has no sidecar; the orphan c.jpg.xmp in "to" would become its keywords.
    convert -size 64x64 xc:red $d/src/c.jpg
    echo orphan >$d/to/c.jpg.xmp
    config
    start $d/src/a.jpg; sleep 2
    key ctrl+a
    answer $d/to
    key ctrl+m; until_file $d/to/a.jpg; sleep 1
    quit
    side="$(ls $d/src | tr '\n' ' ')$(cat $d/to/a.jpg.xmp) $(ls $d/to | tr '\n' ' ')"
    # A folder the file cannot be taken out of: linked to the other, then not unlinked.
    convert -size 64x64 xc:red $d/ro/b.jpg
    chmod 555 $d/ro
    start $d/ro/b.jpg; sleep 2
    answer $d/to2
    key ctrl+m; sleep 2
    quit
    chmod 755 $d/ro
    kept="$(ls $d/ro) $(ls $d/to2 | wc -l)"
    if [ $v = new ]; then
        expect "the photos and the sidecar stay, the other files' sidecars as they were" "$side" "a.jpg a.jpg.xmp c.jpg orphan a.jpg.xmp c.jpg.xmp "
        expect "the file still in its read-only folder, no second name in the other" "$kept" "b.jpg 0"
    else note "old: $side; $kept"; fi
done

section "Lightroom's navn.xmp moved, trashed and put back with navn.cr2, not with navn.jpg"
for v in new old; do
    as_version $v
    d=/tmp/adobe-sidecar-$v
    mkdir -p $d/sub
    # A camera's RAW file has its JPEG preview inside; this one is only the preview.
    convert -size 64x64 xc:red jpg:$d/navn.cr2
    convert -size 64x64 xc:blue $d/navn.jpg
    echo "Lightroom's" >$d/navn.xmp
    config confirm_trash=false
    start $d/navn.cr2; sleep 2
    answer $d/sub; key ctrl+m; until_file $d/sub/navn.cr2; sleep 1
    moved="$(ls $d/sub | tr '\n' ' ')"
    key ctrl+z; until_file $d/navn.cr2; sleep 1
    back="$(ls $d | tr '\n' ' ')"
    key Home; key ctrl+d; until_gone $d/navn.cr2; sleep 1
    trashed="$(ls $d | tr '\n' ' ')"
    key ctrl+z; until_file $d/navn.cr2; sleep 1
    quit
    restored="$(ls $d | tr '\n' ' ')"
    if [ $v = new ]; then
        expect "moved with the RAW file" "$moved" "navn.cr2 navn.xmp "
        expect "the move undone with it" "$back" "navn.cr2 navn.jpg navn.xmp sub "
        expect "to the trash with the RAW file, the JPEG left alone" "$trashed" "navn.jpg sub "
        expect "put back from the trash with it" "$restored" "navn.cr2 navn.jpg navn.xmp sub "
    else note "old: $moved; $back; $trashed; $restored"; fi
done

section "a collection renamed by another program while shown: not made again under its old name"
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
    arrived=$(keys_arrive)
    key Delete; sleep 1                      # the shown one out of the collection: a save
    quit
    got="$(cd $d && ls *.gqv | tr '\n' ' ')"
    if [ $v = new ]; then expect "only Holiday.gqv" "$got" "Holiday.gqv "
        expect "the keys reach the window (the control)" "$arrived" yes
    else note "old: $got"; fi
done

section "a collection renamed, then moved to the trash: its images stay"
d=/tmp/coll
mkdir -p $d $XDG_DATA_HOME/glimt/collections
convert -size 300x200 xc:red $d/a.jpg
c=$XDG_DATA_HOME/glimt/collections
printf '#GQview collection\n"%s"\n' $d/a.jpg >$c/Old.gqv
config
start $c/Old.gqv
sleep 2
menu File "Rename Collection…"
key ctrl+a; xdotool type New; key Return
renamed=$(ls $c | tr '\n' ' ')
menu File "Move Collection to Trash"; sleep 1
asked=$(ls $c | tr '\n' ' ')               # it asks first, and Enter answers
key Return; sleep 2
shot collection-trashed
quit
expect "renamed in its folder" "$renamed" "New.gqv "
expect "kept while it asks" "$asked" "New.gqv "
expect "the collection in the trash" "$(ls $c | wc -l) $(ls $H/.local/share/Trash/files)" "0 New.gqv"
expect "its image stays" "$(ls $d)" "a.jpg"
section "Ctrl+Z: a file back from the trash, a renamed one back to its old name"
for v in new old; do
    as_version $v
    # In the home folder, where gio has a trash.
    d=$H/undo-$v
    rm -rf $d $H/.local/share/Trash && mkdir -p $d $XDG_STATE_HOME/glimt
    convert -size 300x200 xc:red $d/a.jpg
    convert -size 300x200 xc:blue $d/b.jpg
    # An old sidecar of the same name, of another a.jpg, trashed an hour ago; and a.jpg's stars.
    echo '<x:xmpmeta>old</x:xmpmeta>' >$d/a.jpg.xmp
    gio trash $d/a.jpg.xmp
    touch -d '1 hour ago' $H/.local/share/Trash/info/a.jpg.xmp.trashinfo
    printf '0\t4\t%s/a.jpg\n' $d >$XDG_STATE_HOME/glimt/marks
    config confirm_trash=false
    start $d/a.jpg
    sleep 2
    key ctrl+d; sleep 2              # a.jpg to the trash
    trashed=$(ls $d | tr '\n' ' ')
    key ctrl+z; sleep 2
    back=$(ls $d | tr '\n' ' ')
    left=$(ls $H/.local/share/Trash/files 2>/dev/null | tr '\n' ' ')
    key ctrl+r; key ctrl+a; xdotool type c.jpg; key Return; sleep 2
    renamed=$(ls $d | tr '\n' ' ')
    key ctrl+z; sleep 2
    unrenamed=$(ls $d | tr '\n' ' ')
    key ctrl+z; sleep 1              # nothing left to undo: the old name stays
    shot undo-$v
    quit
    stars=$(grep -c "^0	4	$d/a.jpg\$" $XDG_STATE_HOME/glimt/marks 2>/dev/null)
    if [ $v = new ]; then
        expect "a.jpg in the trash (the control)" "$trashed" "b.jpg "
        expect "Ctrl+Z brings a.jpg back from the trash, not another file's old sidecar" "$back" "a.jpg b.jpg "
        expect "only that old sidecar left in the trash" "$left" "a.jpg.xmp "
        expect "a.jpg has its stars again" "$stars" "1"
        # b.jpg shown since a.jpg went: renamed, it is the one.
        expect "renamed (the control)" "$renamed" "a.jpg c.jpg "
        expect "Ctrl+Z gives the old name back, and once only" "$(ls $d | tr '\n' ' ')" "a.jpg b.jpg "
    else note "old: trashed $trashed, after Ctrl+Z $back, stars $stars; renamed $renamed, after $unrenamed"; fi
done

section "File → Set as Wallpaper asks the desktop portal, with the file's URI"
d="/tmp/wall paper"
mkdir -p "$d"
convert -size 300x200 xc:red "$d/a'b.jpg"
# A stand-in gdbus that keeps what it was asked.
printf '#!/bin/sh\nprintf "%%s\\n" "$@" >/tmp/gdbus-args\n' >/tmp/stub/gdbus
chmod +x /tmp/stub/gdbus
rm -f /tmp/gdbus-args
config
start "$d/a'b.jpg"
sleep 2
menu File "Set as Wallpaper"; sleep 2
quit
expect "the portal's method" "$(grep -c '^org.freedesktop.portal.Wallpaper.SetWallpaperURI$' /tmp/gdbus-args)" 1
expect "the file as a quoted URI" "$(grep '^"file' /tmp/gdbus-args)" "\"file:///tmp/wall%20paper/a'b.jpg\""
section "File → Open Recent opens a folder of before, also after a restart"
for k in 1 2; do mkdir -p /tmp/recent-$k && convert -size 300x200 xc:red /tmp/recent-$k/a$k.jpg; done
rm -f $XDG_STATE_HOME/glimt/recent
config
start /tmp/recent-1
sleep 2
remote open /tmp/recent-2 >/dev/null 2>&1; sleep 2
quit
expect "both kept, the newest first" "$(cat $XDG_STATE_HOME/glimt/recent | tr '\n' ' ')" "/tmp/recent-2 /tmp/recent-1 "
start /tmp/recent-2
sleep 2
menu File "Open Recent" /tmp/recent-1; sleep 2
shown=$(remote current | tr -d '\0')
shot open-recent
quit
expect "the folder from the menu" "$shown" "/tmp/recent-1/a1.jpg"
# Off in Preferences: the list kept so far goes, and none is written.
config keep_recent=false
start /tmp/recent-2
sleep 2
remote open /tmp/recent-1 >/dev/null 2>&1; sleep 2
quit
expect "no list with Keep a list of recent folders off" "$(ls $XDG_STATE_HOME/glimt/recent 2>&1 | grep -c 'No such')" 1
section "Checkerboard behind transparent images: two greys in turn, off by default"
d=/tmp/checker
mkdir -p $d/clear $d/red
convert -size 300x200 xc:none $d/clear/a.png
convert -size 300x200 xc:red $d/red/a.png
# Grey (two of them in squares of 8 points) or black at X,Y.
checked() {
    set -- $(pixel $1 $2 $3)
    if [ $1 -gt 90 ] && [ $1 -lt 165 ] && [ $1 = $2 ] && [ $2 = $3 ]; then echo "grey$1"
    elif [ $1 -lt 10 ] && [ $2 -lt 10 ] && [ $3 -lt 10 ]; then echo black
    else echo "$*"; fi
}
for v in new old; do
    as_version $v
    for c in on off; do
        [ $c = on ] && config checkerboard=true || config
        start $d/clear
        sleep 2
        shot checker-$c-$v
        quit
    done
done
as_version new
config checkerboard=true
start $d/red
sleep 2
shot checker-red
quit
a=$(checked checker-on-new 760 412)
b=$(checked checker-on-new 768 412)
expect "a grey square" "${a%%[0-9]*}" grey
differ "the next square another grey" "$a" "$b"
expect "black without the setting" "$(checked checker-off-new 760 412)" black
expect "an opaque image as before" "$(colour checker-red 760 412)" red
note "old: $(checked checker-on-old 760 412) with the setting, as it has none"
section "a lossy JPEG XL of 6000x4000, as a camera makes one: shown within its memory limit"
for v in new old; do
    as_version $v
    d=/tmp/jxl-$v
    mkdir -p $d
    ffmpeg -v error -f lavfi -i color=c=red:s=6000x4000 -frames:v 1 -c:v libjxl $d/big.jxl
    config
    start $d/big.jxl
    for i in $(seq 120); do grep -q "first image" /tmp/glimt.err && break; sleep 0.5; done
    shot jxl-24mp-$v
    shown=$(grep -q "first image" /tmp/glimt.err && echo yes)
    quit
    if [ $v = new ]; then expect "the 24 MP JPEG XL shown" "$shown" yes
    else note "old: shown '$shown'"; fi
done
section "Shift+C crops: a part dragged over the image, saved beside it with Enter; Esc saves none"
for v in new old; do
    as_version $v
    d=/tmp/crop-$v
    mkdir -p $d
    # Left half red, right half blue.
    convert -size 150x200 xc:red -size 150x200 xc:blue +append $d/a.png
    sum=$(md5sum <$d/a.png)
    config
    start $d
    sleep 2
    # The right half, inside the image (the view fills 1040 wide around x 760).
    key shift+c; sleep 0.5
    crop_part
    key Escape
    [ $v = new ] && after_esc=$(ls $d | tr '\n' ' ')
    key shift+c; sleep 0.5
    crop_part
    key Return
    until_file $d/a-cropped.png 5
    [ $v = new ] && { files=$(ls $d | tr '\n' ' '); same=$([ "$(md5sum <$d/a.png)" = "$sum" ] && echo yes); }
    quit
    [ $v = old ] && note "old: $(ls $d | tr '\n' ' ')"
done
d=/tmp/crop-new
expect "Esc saves nothing" "$after_esc" "a.png "
expect "Enter saves a copy beside it" "$files" "a-cropped.png a.png "
expect "the image itself unchanged" "$same" yes
[ -e $d/a-cropped.png ] && size=$(identify -format "%w %h" $d/a-cropped.png)
note "cropped: ${size:-none}"
# 400 x 400 points of a 300 x 200 image shown 1040 wide: about 115 pixels each way.
set -- ${size:-0 0}
expect "about 115 x 115 pixels" "$([ $1 -ge 105 ] && [ $1 -le 125 ] && [ $2 -ge 105 ] && [ $2 -le 125 ] && echo yes)" yes
expect "the right half: blue" "$([ -e $d/a-cropped.png ] && side $d/a-cropped.png 10,10)" blue

section "a crop whose image is deleted meanwhile saves nothing, not the next image's part"
for v in new old; do
    as_version $v
    d=/tmp/crop-gone-$v
    mkdir -p $d
    convert -size 300x200 xc:red $d/a.png
    convert -size 300x200 xc:blue $d/b.png
    config
    start $d/a.png
    sleep 2
    key shift+c; sleep 0.5
    crop_part
    shot crop-gone-chosen-$v
    # Another program deletes a.png; the folder is read again, b.png is shown in its place.
    rm $d/a.png
    until_current $d/b.png
    key Return; sleep 2
    files=$(ls $d | tr '\n' ' ')
    # The control: a crop of b.png chosen now is saved by Enter.
    key shift+c; sleep 0.5; crop_part; key Return; until_file $d/b-cropped.png 5
    quit
    if [ $v = new ]; then expect "nothing saved for b.png" "$files" "b.png "
        expect "a crop chosen on b.png saved (the control)" "$(ls $d | grep -c cropped)" 1
    else note "old: $files"; fi
done

section "Auto Rotate switched during a crop ends it: the part was chosen on another view"
for v in new old; do
    as_version $v
    d=/tmp/crop-rotate-$v
    mkdir -p $d
    convert -size 300x200 xc:red $d/a.png
    config
    start $d/a.png
    sleep 2
    key shift+c; sleep 0.5
    crop_part
    key alt+x; sleep 1
    key Return; sleep 2
    files=$(ls $d | tr '\n' ' ')
    # The control: a crop chosen after the change is saved by Enter.
    key shift+c; sleep 0.5; crop_part; key Return; until_file $d/a-cropped.png 5
    quit
    if [ $v = new ]; then expect "nothing saved after Auto Rotate changed" "$files" "a.png "
        expect "a crop chosen after it saved (the control)" "$(ls $d | grep -c cropped)" 1
    else note "old: $files"; fi
done

section "a playlist opened by its name in its own folder shows its images"
for v in new old; do
    as_version $v
    d=/tmp/list-relative-$v
    mkdir -p $d
    convert -size 64x64 xc:red $d/a.png
    printf 'a.png\n' >$d/show.m3u
    config
    cd $d; start show.m3u; cd - >/dev/null
    shown=$(until_current $d/a.png 5 && echo yes)
    quit
    if [ $v = new ]; then expect "the list's image shown" "$shown" yes
    else note "old: ${shown:-nothing shown}"; fi
done

section "Ctrl+Shift+S saves as: JPEG at half the size, the extension added, the image kept"
for v in new old; do
    as_version $v
    d=/tmp/save-as-$v
    mkdir -p $d/out
    convert -size 400x200 xc:red $d/a.png
    sum=$(md5sum <$d/a.png)
    # The stand-in file dialog: says what name it was offered, answers a name without extension.
    printf '#!/bin/sh\necho "$@" >/tmp/zenity-args\necho "%s"\n' $d/out/b >/tmp/stub/zenity
    chmod +x /tmp/stub/zenity
    rm -f /tmp/zenity-args
    config
    start $d
    sleep 2
    key ctrl+shift+s; sleep 1
    menu "Percent" "As the original (PNG)" "JPEG" "Save…"
    until_file $d/out/b.jpg 10
    [ $v = new ] && { files=$(ls $d/out | tr '\n' ' '); size=$(identify -format "%m %w %h" $d/out/b.jpg 2>/dev/null)
        offered=$(grep -o 'a-200.jpg' /tmp/zenity-args); same=$([ "$(md5sum <$d/a.png)" = "$sum" ] && echo yes); }
    quit
    [ $v = old ] && note "old: $(ls $d/out | tr '\n' ' ')"
done
expect "offered a-200.jpg beside the image" "$offered" a-200.jpg
expect "saved as b.jpg" "$files" "b.jpg "
expect "a JPEG of 200 x 100" "$size" "JPEG 200 100"
expect "the image itself unchanged" "$same" yes

section "File → Copy for Sharing… with Remove All: no place, camera or keywords; pixels and orientation kept"
for v in new old; do
    as_version $v
    d=/tmp/sharing-$v
    mkdir -p $d/out
    convert -size 64x48 gradient:red-blue -quality 92 $d/a.jpg
    convert -size 64x48 gradient:green-blue $d/b.png
    exiftool -q -q -overwrite_original -Orientation#=6 -Make=SecretMake -SerialNumber=SN-424242 \
        -GPSLatitude=59.9 -GPSLatitudeRef=N -XMP-dc:Subject=secretword $d/a.jpg $d/b.png
    sums=$(md5sum $d/a.jpg $d/b.png)
    answer $d/out
    config
    start $d
    sleep 2
    remote select $d/a.jpg $d/b.png >/dev/null
    sleep 1
    menu File "Copy for Sharing…"; sleep 3
    shot sharing-$v
    menu "Remove All" "Copy…"
    until_file $d/out/b.png 10
    sleep 1
    quit
    if [ $v = new ]; then
        expect "both images copied" "$(ls $d/out | tr '\n' ' ')" "a.jpg b.png "
        expect_empty "no place, make, serial number or keyword anywhere in the copies" \
            "$(grep -alE 'SecretMake|SN-424242|secretword' $d/out/*)"
        for f in a.jpg b.png; do
            expect_empty "$f: exiftool finds no place, make, serial number or keyword" \
                "$(exiftool -s3 -GPSLatitude -Make -SerialNumber -Subject $d/out/$f)"
            expect "$f: the orientation kept" "$(exiftool -s3 -n -Orientation $d/out/$f)" 6
            expect "$f: the same pixels" \
                "$(compare -metric AE $d/$f $d/out/$f null: 2>&1 | cut -d' ' -f1)" 0
        done
        expect "the originals unchanged" "$(md5sum $d/a.jpg $d/b.png)" "$sums"
        expect "Remove All kept for the next time" \
            "$(grep -c '^group:' $XDG_STATE_HOME/glimt/sharing 2>/dev/null)" 8
    else note "old: $(ls $d/out | tr '\n' ' ')"; fi
done

section "File → Copy for Sharing… of real photos (samples.txt): no place, serial or keyword left; gain maps kept"
# The samples are a Pixel's Ultra HDR, an iPhone's HDR with Apple's gain map, a DJI drone's
# photo, a Lightroom export with its keyword tree, one with content credentials and one with
# digiKam's keyword list. Only NEW is run: the section checks what a copy holds, which tells
# nothing new about OLD.
note "old: not run, the copies of NEW only are checked"
d=/tmp/sharing-real
mkdir -p $d/in $d/all $d/place
for name in ultrahdr-pixel.jpg iphone-gain-map.jpg dji-drone.jpg lightroom-keywords.jpg c2pa.jpg \
    digikam-tags.jpg; do
    if [ -f /samples/$name ]; then cp /samples/$name $d/in/; else fail "$name not downloaded"; fi
done
# copy_shared FOLDER GROUP…: copies every sample into FOLDER, with Remove All when no GROUP is
# named, and else with each GROUP's box ticked. The choice starts empty each time.
copy_shared() {
    local out=$1; shift
    rm -f $XDG_STATE_HOME/glimt/sharing
    answer $out
    config
    start $d/in
    sleep 2
    remote select $d/in/* >/dev/null
    sleep 1
    menu File "Copy for Sharing…"
    until_ok '[ -n "$(where "Remove All")" ]' 30
    if [ $# = 0 ]; then menu "Remove All"; else menu "$@"; fi
    shot sharing-real-$(basename $out)
    menu "Copy…"
    for name in $(ls $d/in); do until_file $out/$name 30; done
    sleep 1
    quit
}
# The tags of FILE and of the images after its main one, one to a line, as [Group:Group] Tag,
# without the file's name and folder, which are not in the file.
all_tags() { exiftool -a -ee3 -G0:1 -s "$1" 2>/dev/null | grep -v '^\[File:'; }
# What Remove All and Place must leave out, as all_tags prints it.
place_tags='gps|latitude|longitude|altitude'
private_tags="$place_tags|serial|makernote|^\[(MakerNotes|JUMBF)|keywords|hierarchicalsubject|tagslist|\] *Subject "
# What a copy keeps of an iPhone's maker note on purpose: the two values its gain map is shown
# with, and nothing else of the note.
hdr_tags='^\[MakerNotes:Apple\] (HDRHeadroom|HDRGain) '
copy_shared $d/all
copy_shared $d/place Place
samples=$(ls $d/in | tr '\n' ' ')
expect "every sample copied with Remove All" "$(ls $d/all | tr '\n' ' ')" "$samples"
expect "every sample copied with Place" "$(ls $d/place | tr '\n' ' ')" "$samples"
for name in $(ls $d/in); do
    in=$d/in/$name all=$d/all/$name place=$d/place/$name
    expect_empty "$name, Remove All: no place, serial number, maker note, credentials or keyword" \
        "$(all_tags $all | grep -iE "$private_tags" | grep -vE "$hdr_tags")"
    expect "$name, Remove All: the orientation kept" \
        "o=$(exiftool -s3 -n -Orientation $all)" "o=$(exiftool -s3 -n -Orientation $in)"
    expect "$name, Remove All: the colour profile kept" \
        "p=$(exiftool -s3 -ProfileDescription $all)" "p=$(exiftool -s3 -ProfileDescription $in)"
    expect_empty "$name, Place: no place of any kind, in no image of the file" \
        "$(all_tags $place | grep -iE "$place_tags")"
    expect "$name, Place: the camera's make kept" \
        "make=$(exiftool -s3 -Make $place)" "make=$(exiftool -s3 -Make $in)"
    # -quiet, as ImageMagick warns of the XMP of Lightroom's export after the number.
    for copy in $all $place; do
        expect "$name, $(basename $(dirname $copy)): the main image's pixels unchanged" \
            "$(compare -quiet -metric AE $in $copy null: 2>&1 | cut -d' ' -f1)" 0
    done
done
# The gain maps: the index of the images after the main one, and the gain map marked as such.
for copy in $d/all $d/place; do
    kind=$(basename $copy)
    expect "ultrahdr-pixel.jpg, $kind: two images in the MPF index" \
        "$(exiftool -s3 -NumberOfImages $copy/ultrahdr-pixel.jpg)" 2
    expect "ultrahdr-pixel.jpg, $kind: the gain map's hdrgm marking kept" \
        "$(exiftool -a -ee3 -G3 -s -XMP-hdrgm:GainMapMax $copy/ultrahdr-pixel.jpg | grep -c Doc)" 1
    expect "iphone-gain-map.jpg, $kind: two images in the MPF index" \
        "$(exiftool -s3 -NumberOfImages $copy/iphone-gain-map.jpg)" 2
    expect "iphone-gain-map.jpg, $kind: the gain map's Apple marking kept" \
        "$(exiftool -a -ee3 -s3 -XMP-apdi:AuxiliaryImageType $copy/iphone-gain-map.jpg)" \
        urn:com:apple:photo:2020:aux:hdrgainmap
    # The gain map is shown as bright as the original only with the maker note's headroom.
    expect "iphone-gain-map.jpg, $kind: the maker note's HDR headroom kept" \
        "$(exiftool -s3 -MakerNotes:HDRHeadroom $copy/iphone-gain-map.jpg)" \
        "$(exiftool -s3 -MakerNotes:HDRHeadroom $d/in/iphone-gain-map.jpg)"
done
# Place alone keeps the keywords: digiKam's list, which Remove All takes out. digiKam 4 wrote
# it in a namespace that exiftool does not name XMP-digiKam, so it is read as XMP:TagsList.
tags_list() { exiftool -q -q -s3 -XMP:TagsList "$1"; }
expect "digikam-tags.jpg: the original holds digiKam's keyword list" \
    "$(tags_list $d/in/digikam-tags.jpg)" Adrien
expect "digikam-tags.jpg, Place: digiKam's keyword list kept" \
    "$(tags_list $d/place/digikam-tags.jpg)" Adrien
expect_empty "digikam-tags.jpg, Remove All: digiKam's keyword list left out" \
    "$(tags_list $d/all/digikam-tags.jpg)"
expect "the originals unchanged" "$(cd $d/in && md5sum * | md5sum)" \
    "$(cd /samples && md5sum $(ls $d/in) | md5sum)"

section "/ puts the cursor in the search field, without typing itself there"
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

section "a folder named like an image, a pipe and a link to /dev/zero: listed as they are"
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
arrived=$(keys_arrive)
alive=$(kill -0 $GPID 2>/dev/null && echo yes || echo no)
mb=$(awk '/RssAnon/ {print int($2 / 1024)}' /proc/$GPID/status 2>/dev/null)
quit
expect "the keys reach the window (the control)" "$arrived" yes
expect "Glimt still runs" "$alive" yes
if [ "${mb:-9999}" -lt 400 ]; then pass "memory stays small: $mb MB"; else fail "memory: ${mb:-?} MB"; fi

# Up to 0.72 the PDF replaced the image named, once every image was read. From 0.73 a name
# Glimt lists as an image is refused, so the image and its place in the list are kept.
section "printing to a PDF named as one of the images printed: refused, the image kept"
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
    sleep 4
    # The control: the same keys print to a name that is no image.
    answer $d/out.pdf; key ctrl+a; key ctrl+p; sleep 1; key Return
    until_ok "pdfinfo $d/out.pdf" 30
    quit
    kind=$(identify -format '%m' "$d/a.jpg[0]" 2>/dev/null)
    if [ $v = new ]; then expect "a.jpg is still the JPEG" "$kind" JPEG
        expect "out.pdf printed (the control)" "$(pdfinfo $d/out.pdf >/dev/null 2>&1 && echo yes)" yes
    else note "old: a.jpg is $kind"; fi
done

section "printing to a PDF when no image can be read: the file chosen is left as it was"
d=/tmp/print-none
for v in new old; do
    as_version $v
    rm -rf $d && mkdir -p $d
    echo "not a picture" >$d/a.jpg
    convert -size 64x64 xc:red $d/b.jpg
    echo "keep" >$d/out.pdf
    config
    start $d/a.jpg
    sleep 2
    answer $d/out.pdf; key ctrl+p; sleep 1; key Return; sleep 4
    # The control: b.jpg, which can be read, is printed by the same keys.
    key Right; answer $d/b.pdf; key ctrl+p; sleep 1; key Return
    until_ok "pdfinfo $d/b.pdf" 30
    quit
    got=$(cat $d/out.pdf | head -c 4)
    if [ $v = new ]; then expect "out.pdf untouched" "$got" "keep"
        expect "b.jpg printed (the control)" "$(pdfinfo $d/b.pdf 2>/dev/null | awk '/^Pages:/ {print $2}')" 1
    else note "old: $got"; fi
done

section "browsing past images writes nothing: a keyword with a comma, a comment with a newline"
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
    config info_panel=true
    start $d/a.jpg
    sleep 2
    key space; sleep 2; key space; sleep 2
    quit
    got="$(cmp -s $d/a.jpg.xmp /tmp/browse-a.xmp && echo same || echo changed) $(ls $d/b.jpg.xmp 2>/dev/null | wc -l)"
    if [ $v = new ]; then expect "the sidecar kept, no new one" "$got" "same 0"
    else note "old: $got"; fi
done

section "an exif: search after a keyword was set finds it, without indexing all again"
d=/tmp/exifsearch
mkdir -p $d
convert -size 300x200 xc:red $d/a.jpg
convert -size 300x200 xc:blue $d/b.jpg
printf 'Zebra\n' >$XDG_CONFIG_HOME/glimt/keywords
config info_panel=true
start $d/b.jpg
sleep 2
search() { click 120 284; key ctrl+a; xdotool type "$1"; sleep 3; }
search exif:zebra
shot exif-search-0
before=$(colour exif-search-0 57 337)
key ctrl+a; key BackSpace; key Escape; sleep 1
menu Zebra; sleep 2   # the Zebra box, for b.jpg
search exif:zebra
shot exif-search-1
quit
expect "nothing before, b.jpg after" "$before $(colour exif-search-1 57 337)" "other blue"

section "Rename Many cut short by a crash: the old names put back at the next start"
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
menu "Put the old names back"; sleep 3
shot cut-short
quit
# a.jpg was the red one: more than half red, as JPEG rounds.
r=$(convert $d/a.jpg -format "%[fx:p{150,100}.r>0.5]" info: 2>/dev/null)
got="$(ls -A $d | tr '\n' ' ')$r $(ls -A $XDG_STATE_HOME/glimt/renames | wc -l)"
expect "a.jpg (red) and b.jpg back, the plan gone" "$got" "a.jpg b.jpg 1 0"

section "Rename Many cut short, finished at the next start: the marks follow the files"
d=/tmp/cutshort-finish
mkdir -p $d $XDG_STATE_HOME/glimt/renames
convert -size 300x200 xc:red $d/a.jpg
printf '%s\0' $d/a.jpg $d/c.jpg >$XDG_STATE_HOME/glimt/renames/crashed    # a.jpg -> c.jpg, not started
printf '1\t0\t%s/a.jpg\n' $d >$XDG_STATE_HOME/glimt/marks
config
start $d
sleep 3
shot cut-short-finish
menu "Finish the renaming"; sleep 3
quit
got="$(ls -A $d | tr '\n' ' ')$(grep -c 'c\.jpg' $XDG_STATE_HOME/glimt/marks) $(grep -c 'a\.jpg' $XDG_STATE_HOME/glimt/marks)"
expect "c.jpg there, its mark on c.jpg, none on a.jpg" "$got" "c.jpg 1 0"

section "size and date columns with thumbnails: the list wide enough for the names"
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
    # Thumbnail, name and both columns: the list ends past x=465, where the red image was. The
    # image itself is shown at x=1200, so not red at 465 is the list, not an empty window.
    if [ $v = new ]; then
        expect "the image shown beside the list" "$(colour columns-new 1200 400)" red
        differ "the list has room for the name" "$(colour columns-new 465 400)" red
    else note "old, at x=465: $(colour columns-old 465 400)"; fi
done

section "thumbnails without columns: the list is never dragged narrower than a thumbnail"
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
    # A thumbnail is about 100 wide: at x=150 the list is still there, not the red view. The
    # list's narrowest is about 190, so red at x=220 shows the drag made it narrower than 240.
    if [ $v = new ]; then
        expect "the list dragged narrower" "$(colour narrow-new 220 400)" red
        differ "the list keeps a thumbnail's width" "$(colour narrow-new 150 400)" red
    else note "old, at x=150: $(colour narrow-old 150 400)"; fi
done

section "the Pan view's calendar: a day with images shows its first one"
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
menu "EXIF date"; sleep 2
menu Calendar; sleep 2
shot calendar-new
quit
# July 2025 starts on a Tuesday: the 16th is the third Wednesday, with the blue picture.
expect "the calendar shows the day's first picture" "$(colour calendar-new 290 407)" blue

section "the image info template: one whose fields are all empty shows no box"
d=/tmp/osd
mkdir -p $d
convert -size 1600x1000 xc:red $d/a.jpg
for t in empty:%comment% name:%name%; do
    config "full_screen_info_template=${t#*:}"
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

section "printing 80 images of 3000x2000 to a PDF"
print_pictures
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
    # Some 900 MB each: /tmp is the container's disk where it is no tmpfs.
    rm -f $pdf
done

section "printing a contact sheet on landscape paper, with image and page text from templates"
d=/tmp/print-sheet
mkdir -p $d
for c in red lime blue yellow cyan; do convert -size 600x400 xc:$c $d/$c.jpg; done
as_version new
pdf=/tmp/print-sheet.pdf
config print_per_page=4 print_orientation=landscape "print_template=%name% (%position%)" \
    "print_footer=Page %page% of %pages%" print_header=Summer
start $d
answer $pdf; key ctrl+a; key ctrl+p; sleep 2; key Return
for i in $(seq 60); do [ -s $pdf ] && sleep 1 && break; sleep 1; done
quit
expect "two pages" "$(pdfinfo $pdf 2>/dev/null | awk '/^Pages/ {print $2}')" 2
expect "landscape paper" "$(pdfinfo $pdf 2>/dev/null | awk '/^Page size/ {print ($3 > $5) ? "landscape" : "portrait"}')" landscape
text=$(pdftotext $pdf - 2>/dev/null | tr '\n' ' ')
expect "page numbers, header and image text" \
    "$(echo "$text" | grep -c 'Page 2 of 2').$(echo "$text" | grep -c 'Summer').$(echo "$text" | grep -c 'yellow.jpg (5/5)')" "1.1.1"

section "Enter in the print dialog's page text ends the typing, not the dialog"
for v in new old; do
    as_version $v
    d=/tmp/print-enter-$v
    mkdir -p $d
    convert -size 300x200 xc:red $d/a.jpg
    pdf=/tmp/print-enter-$v.pdf
    config
    start $d
    answer $pdf; key ctrl+p; sleep 2
    menu "Top of each page:"
    xdotool type Summer; key Return; sleep 3
    typed=$([ -s $pdf ] && echo printed || echo kept)
    # The dialog still prints: Enter again, with no field being typed in.
    key Return; sleep 3
    then=$([ -s $pdf ] && echo printed || echo kept)
    quit
    if [ $v = new ]; then
        expect "Enter in the page text: nothing printed" "$typed" kept
        expect "Enter again: printed" "$then" printed
    else note "old: $typed, then $then"; fi
done

section "closed while printing to a file: Glimt ends once the PDF is whole"
print_pictures
for v in new old; do
    as_version $v
    pdf=/tmp/print-closed-$v.pdf
    config thumbnails=false
    start $src
    sleep 2
    answer $pdf; key ctrl+a; key ctrl+p; sleep 1; key Return
    sleep 1
    timeout 30 $BIN --remote-all quit >/dev/null 2>&1
    for i in $(seq 300); do kill -0 $GPID 2>/dev/null || break; sleep 1; done
    kill $GPID 2>/dev/null
    pages=$(pdfinfo $pdf 2>/dev/null | awk '/^Pages/ {print $2}')
    left=$(ls -A /tmp | grep -c 'glimt-tmp')
    if [ $v = new ]; then
        expect "the PDF whole, 80 pages" "$pages" "80"
        expect "no temporary file left" "$left" 0
    else note "old: pages '$pages', temporary files $left"; fi
    rm -f /tmp/*.glimt-tmp /tmp/.glimt-tmp-* $pdf
done

section "screen profile: Adobe RGB, so sRGB red is shown less red"
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

section "the desktop's screen profile: a stand-in gdbus answers for colord with Adobe RGB"
d=/tmp/desktop-profile
mkdir -p $d
convert -size 600x400 xc:red $d/red.png
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
r=$(red desktop-true 760 400)
[ "$r" -lt 240 ] && pass "the desktop's Adobe RGB: red shown as $r" ||
    fail "the desktop's Adobe RGB: red shown as $r"
expect "the desktop's profile left out: red as it is" "$(red desktop-false 760 400)" "255"

section "images without a profile, Adobe RGB from EXIF, and the image's own profile left out"
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
expect "its own profile left out: as sRGB" "$(show icc.png use_image_profile=false)" "$plain"

section "the window title: the template in Preferences, this window's own, and back"
d=/tmp/title
mkdir -p $d
convert -size 64x64 xc:red $d/a.png
config 'window_title=%app%: %name%'
start $d/a.png
from_setting=$(xdotool getwindowname $WIN)
menu Window "Set Window Title…"
xdotool type 'Left %name%'; key Return
from_menu=$(xdotool getwindowname $WIN)
remote title 'Right: %folder%'; sleep 1
from_remote=$(xdotool getwindowname $WIN)
remote title; sleep 1
back=$(xdotool getwindowname $WIN)
quit
expect "the template in Preferences" "$from_setting" "Glimt: a.png"
expect "Window → Set Window Title…" "$from_menu" "Left a.png"
expect "glimt --remote title" "$from_remote" "Right: $d"
expect "glimt --remote title without text: Preferences' again" "$back" "Glimt: a.png"

section "two windows named left: identify in both, one renamed right by its process ID, next to right only"
for v in new old; do
    as_version $v
    d=/tmp/shared-name-$v
    mkdir -p $d
    for f in a b c; do convert -size 64x64 xc:red $d/$f.png; done
    # With remote control off, the windows listen only because they have a name.
    config 'window_title=%id%' remote_control=false
    start --name left $d/a.png; first_win=$WIN
    start --name left $d/a.png; second=$GPID; second_win=$WIN
    titles="$(xdotool getwindowname $first_win) | $(xdotool getwindowname $second_win)"
    timeout 30 $BIN --name left --remote identify >/dev/null 2>&1; identified=$?
    # The second window shares the name, so its process ID tells it apart from the first.
    timeout 30 $BIN --name $second --remote name right >/dev/null 2>&1; renamed=$?
    sleep 1
    renamed_titles="$(xdotool getwindowname $first_win) | $(xdotool getwindowname $second_win)"
    timeout 30 $BIN --name right --remote next >/dev/null 2>&1; sleep 1
    left=$(timeout 30 $BIN --name left --remote current | tr '\0' ' ')
    right=$(timeout 30 $BIN --name right --remote current | tr '\0' ' ')
    # Its name taken away, the window no longer listens, by name or by process ID.
    timeout 30 $BIN --name right --remote name >/dev/null 2>&1; unnamed=$?
    sleep 1
    timeout 30 $BIN --name $second --remote current >/dev/null 2>&1; by_id=$?
    timeout 30 $BIN --name right --remote current >/dev/null 2>&1; by_name=$?
    quit
    if [ $v = new ]; then
        expect "each window's %id%" "$titles" "left | left ($second)"
        expect "--remote identify reaches both" "$identified" 0
        expect "--remote name right, sent by process ID" "$renamed $renamed_titles" "0 left | right"
        expect "next moved only the window named right" "$left|$right" "$d/a.png |$d/b.png "
        expect "the name taken away: no longer reached" "$unnamed $by_id $by_name" "0 1 1"
    else note "old: titles '$titles', identify $identified, name $renamed, '$left|$right'"; fi
done

section "a full disk during a Move, a Copy, a turned save and a sidecar write: every file kept, told"
# The root part mounts a tmpfs of 16 MB at /mnt/full for the line below. Each step fills it with a
# hidden file of zeros, all but the room it leaves free.
faults=yes
full=/mnt/full
# leave_free BYTES: the tmpfs full but for BYTES.
leave_free() {
    rm -f $full/.fill
    head -c 20M /dev/zero >$full/.fill 2>/dev/null
    truncate -s -$1 $full/.fill
}
# Pictures without the light red of a notice, which a gradient of blues with noise lacks.
blue_photo() { convert -size $1 gradient:navy-blue -attenuate 0.5 +noise Gaussian -quality 95 $2; }
if ! grep -q " $full tmpfs " /proc/mounts; then
    fail "no tmpfs at $full: run.sh's containers need --privileged to mount it"
else
    for v in new old; do
        as_version $v
        mkdir -p $XDG_STATE_HOME/glimt
        # A Move and a Copy to the full disk: the photo, its sidecar and its stars stay.
        for op in move copy; do
            rm -f $full/.fill
            d=/tmp/full-$op-$v to=$full/$op-$v
            rm -rf $d $to; mkdir -p $d $to
            blue_photo 800x600 $d/a.jpg
            echo "sidecar of a" >$d/a.jpg.xmp
            star_line=$(printf '0\t4\t%s/a.jpg' $d)
            echo "$star_line" >$XDG_STATE_HOME/glimt/marks
            sums="$(cat $d/a.jpg $d/a.jpg.xmp | md5sum)"
            leave_free $(( $(stat -c %s $d/a.jpg) / 2 ))
            config
            start $d/a.jpg
            answer $to
            if [ $op = move ]; then key ctrl+m; else key ctrl+c; fi
            until_notice full-$op-$v; told=$?
            quit
            got="$([ "$(cat $d/a.jpg $d/a.jpg.xmp | md5sum)" = "$sums" ] && echo kept)"
            got="$got,$(ls -A $to | tr '\n' ' '),$(grep -cxF "$star_line" $XDG_STATE_HOME/glimt/marks)"
            got="$got,$([ $told = 0 ] && echo told)"
            if [ $v = new ]; then
                expect "$op to a full disk: photo and sidecar kept, nothing left there, the stars kept, told" \
                    "$got" "kept,,1,told"
            else note "old, $op: $got"; fi
            rm -rf $d $to
        done
        # A turned PNG saved whole, and a JPEG's orientation written into a new copy: neither fits.
        for f in a.png b.jpg; do
            rm -f $full/.fill
            d=$full/turn-$v
            rm -rf $d; mkdir -p $d
            blue_photo 600x400 $d/$f
            convert -size 64x64 xc:blue $d/z.png       # the next image, which saves the turn
            sum=$(md5sum <$d/$f)
            leave_free $(( $(stat -c %s $d/$f) / 2 ))
            config save_orientation=true
            start $d/$f
            key bracketright; key space
            until_notice full-turn-$f-$v; told=$?
            quit
            got="$([ "$(md5sum <$d/$f)" = "$sum" ] && echo kept),$(ls -A $d | tr '\n' ' '),$([ $told = 0 ] && echo told)"
            if [ $v = new ]; then
                expect "$f turned and saved on a full disk: kept as it was, no part left, told" \
                    "$got" "kept,$f z.png ,told"
            else note "old, $f turned: $got"; fi
            rm -rf $d
        done
        # A keyword set with the keyword tree: the sidecar written new does not fit.
        rm -f $full/.fill
        d=$full/sidecar-$v
        rm -rf $d; mkdir -p $d
        convert -size 300x200 xc:blue $d/a.jpg
        cat >$d/a.jpg.xmp <<XMP
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:subject><rdf:Bag><rdf:li>Old</rdf:li></rdf:Bag></dc:subject>
 </rdf:Description></rdf:RDF></x:xmpmeta>
XMP
        sum=$(md5sum <$d/a.jpg.xmp)
        printf 'Animals\n' >$XDG_CONFIG_HOME/glimt/keywords
        leave_free 0
        config info_panel=true
        start $d/a.jpg
        for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
        sleep 1
        menu Animals
        until_notice full-sidecar-$v; told=$?
        quit
        got="$([ "$(md5sum <$d/a.jpg.xmp)" = "$sum" ] && echo kept),$(ls -A $d | tr '\n' ' '),$([ $told = 0 ] && echo told)"
        if [ $v = new ]; then
            expect "a keyword's sidecar written on a full disk: the sidecar kept, no part left, told" \
                "$got" "kept,a.jpg a.jpg.xmp ,told"
        else note "old, the sidecar: $got"; fi
        rm -rf $d
    done
    rm -f $full/.fill
fi

section "a share that removes a file but answers its unlink with an error: a Move and a Rename keep the photo"
# OPS-1. The root part mounts /mnt/lossy for the line below: its unlink of lost-* removes the file
# and answers EIO, and its stat still finds the name for 30 seconds, as a share whose reply was
# lost does while its cache holds the name. It has no RENAME_NOREPLACE, so a Rename there is a
# hard link and an unlink; a Move from it to the local disk is a copy and an unlink. Neither may
# then take back the name that is the only copy left.
faults=yes
lossy=/mnt/lossy
if ! grep -q " $lossy fuse" /proc/mounts; then
    fail "no FUSE file system at $lossy: run.sh's containers need --privileged for /dev/fuse"
    tail -5 /tmp/lossyfs.log 2>/dev/null | sed 's/^/     /'
else
    for v in new old; do
        as_version $v
        back=/opt/lossy/$v to=/tmp/lossy-to-$v
        rm -rf $back $to; mkdir -p $back $to
        convert -size 300x200 xc:blue $back/lost-a.jpg
        convert -size 300x200 xc:lime $back/lost-b.jpg
        a=$(md5sum <$back/lost-a.jpg) b=$(md5sum <$back/lost-b.jpg)
        # copies SUM: how many files hold the photo, on the share's disk or the local one.
        copies() { find $back $to -type f -exec md5sum {} + | grep -c "${1%% *}"; }
        config
        start $lossy/$v/lost-a.jpg
        answer $to
        key ctrl+m
        until_notice lossy-move-$v; told_move=$?
        quit
        start $lossy/$v/lost-b.jpg
        key ctrl+r; sleep 1; key ctrl+a
        xdotool type --delay 30 kept-b.jpg; key Return
        until_notice lossy-rename-$v; told_rename=$?
        quit
        move="$(copies "$a") $([ $told_move = 0 ] && echo told)"
        rename="$(copies "$b") $([ $told_rename = 0 ] && echo told)"
        files="$(ls -A $back | tr '\n' ' ')| $(ls -A $to | tr '\n' ' ')"
        if [ $v = new ]; then
            expect "a Move from the share: the photo still on a disk, the failure told" "$move" "1 told"
            expect "a Rename on the share: the photo still on its disk, the failure told" "$rename" "1 told"
        else note "old: Move $move, Rename $rename (copies left, told); files $files"; fi
        [ $v = new ] && note "new: files $files"
    done
fi

section "a JPEG still being written, turned with Save Orientation on: the photo whole at its name"
# SAVE-1. A copier (a file manager, gphoto2) still appends to a.jpg when the user turns it and
# goes on. A save that renames its copy over the name while the copier writes on into the old
# file leaves the photo cut off at its name.
for v in new old; do
    as_version $v
    d=/tmp/growing-$v whole=/tmp/growing-whole-$v.jpg
    rm -rf $d; mkdir -p $d
    convert -size 2400x1600 gradient:navy-teal -attenuate 0.6 +noise Gaussian -quality 95 $whole
    convert -size 300x200 xc:blue $d/b.jpg
    # Half at once, then 512 bytes every 5 ms: about 15 seconds for the rest of 3 MB.
    python3 -c "
import sys, time
data = open(sys.argv[1], 'rb').read()
with open(sys.argv[2], 'wb') as f:
    half = len(data) // 2
    f.write(data[:half]); f.flush()
    for i in range(half, len(data), 512):
        time.sleep(0.005); f.write(data[i:i + 512]); f.flush()
" $whole $d/a.jpg &
    writer=$!
    until_file $d/a.jpg
    config save_orientation=true
    start $d/a.jpg
    key bracketright; key space      # turned, then the next image, which saves the turn
    writing=$(kill -0 $writer 2>/dev/null && echo writing)
    until_notice growing-$v 4; told=$?
    wait $writer
    sleep 1
    quit
    got="$writing $(identify -format '%#' $d/a.jpg 2>/dev/null) $(stat -c %s $d/a.jpg)"
    want="writing $(identify -format '%#' $whole) $(stat -c %s $whole)"
    if [ $v = new ]; then
        expect "the photo whole at its name, its pixels as the copier wrote them" "$got" "$want"
        note "new: the turn $([ $told = 0 ] && echo told || echo not told)"
    else note "old: $([ "$got" = "$want" ] && echo whole || echo "cut off: $got, whole: $want")"; fi
    rm -rf $d $whole
done

section "Glimt killed (kill -9) during a Move and a Rename Many: every photo under a name, the stars follow"
# EDGES-3: a Move tells the marks where the files went only in its last report.
for v in new old; do
    as_version $v
    mkdir -p $XDG_STATE_HOME/glimt
    # 200 photos of 10 MB, each with a sidecar and three stars, moved from the tmpfs /dev/shm to
    # the disk under /tmp, so each is copied and then unlinked; killed after the tenth.
    d=/dev/shm/killed-$v to=/tmp/killed-to-$v
    rm -rf $d $to; mkdir -p $d $to
    head -c 10000000 /dev/urandom >$d/001.jpg
    for i in $(seq -w 2 200); do ln $d/001.jpg $d/$i.jpg; done
    for i in $(seq -w 1 200); do echo "sidecar $i" >$d/$i.jpg.xmp; done
    for i in $(seq -w 1 200); do printf '0\t3\t%s/%s.jpg\n' $d $i; done >$XDG_STATE_HOME/glimt/marks
    config
    start $d
    sleep 2
    key ctrl+a
    answer $to
    key ctrl+m
    for i in $(seq 400); do [ "$(ls $to | grep -c 'jpg$')" -ge 10 ] && break; sleep 0.05; done
    kill -9 $GPID; wait $GPID 2>/dev/null
    moved=$(ls $to | grep -c 'jpg$')
    start $to; sleep 2; quit         # a new start, which may put things right
    got=$(python3 - $d $to $XDG_STATE_HOME/glimt/marks <<'PY'
import os, sys
d, to, marks = sys.argv[1:]
stars = {}
for line in open(marks):
    fields = line.rstrip("\n").split("\t")
    if len(fields) == 3:
        stars[fields[2]] = fields[1]
named = beside = starred = 0
for i in range(1, 201):
    name = "%03d.jpg" % i
    places = [p for p in (d, to) if os.path.isfile(f"{p}/{name}")]
    named += bool(places)
    beside += any(os.path.isfile(f"{p}/{name}.xmp") for p in places)
    starred += any(stars.get(f"{p}/{name}") == "3" for p in places)
print(named, beside, starred)
PY
)
    hidden=$(ls -A $to | grep -c '^\.')
    rm -rf $d $to
    if [ $v = new ]; then
        expect "killed during a Move: every photo under a name, its sidecar beside it, its stars on it" \
            "$got" "200 200 200"
        note "new: $moved moved when killed, $hidden hidden parts left"
    else note "old: named, sidecar beside, starred: $got ($moved moved when killed, $hidden hidden left)"; fi
    # Rename Many of 10,000 photos, sorted descending to 00001…: each takes the name of another,
    # so all go through hidden names. Each photo ends with its own number, and has i % 5 + 1
    # stars. Killed once 50 are under hidden names.
    d=/tmp/killed-rename-$v
    rm -rf $d $XDG_STATE_HOME/glimt/renames; mkdir -p $d
    convert -size 32x32 xc:blue /tmp/killed-base.jpg
    python3 - $d $XDG_STATE_HOME/glimt/marks <<'PY'
import sys
d, marks = sys.argv[1:]
base = open("/tmp/killed-base.jpg", "rb").read()
with open(marks, "w") as m:
    for i in range(1, 10001):
        open(f"{d}/{i:05d}.jpg", "wb").write(base + b"%d" % i)
        m.write(f"0\t{i % 5 + 1}\t{d}/{i:05d}.jpg\n")
PY
    config sort_ascending=false
    start $d
    sleep 2
    key ctrl+a; key ctrl+r; sleep 1; key ctrl+a
    xdotool type --delay 30 '#####'; sleep 0.4
    xdotool key Return
    python3 -c "
import os, signal, sys, time
d, pid = sys.argv[1], int(sys.argv[2])
end = time.time() + 60
while time.time() < end:
    if sum(n.startswith('.glimt-rename') for n in os.listdir(d)) >= 50:
        break
os.kill(pid, signal.SIGKILL)" $d $GPID
    wait $GPID 2>/dev/null
    cut=$(ls -A $d | grep -c '^\.glimt-rename')
    start $d
    sleep 3
    menu "Finish the renaming"
    until_ok "! ls -A $d | grep -q '^\.' && [ -z \"\$(ls -A $XDG_STATE_HOME/glimt/renames)\" ]" 60
    sleep 1
    quit
    # Named: files NNNNN.jpg; renamed: NNNNN.jpg holds photo 10001 - NNNNN; starred: its stars on it.
    got=$(python3 - $d $XDG_STATE_HOME/glimt/marks <<'PY'
import os, sys
d, marks = sys.argv[1:]
base = len(open("/tmp/killed-base.jpg", "rb").read())
held = {}
for name in os.listdir(d):
    held[f"{d}/{name}"] = int(open(f"{d}/{name}", "rb").read()[base:] or 0)
named = sum(1 for p in held if p.endswith(".jpg") and not os.path.basename(p).startswith("."))
renamed = sum(1 for p, i in held.items() if os.path.basename(p) == f"{10001 - i:05d}.jpg")
hidden = sum(1 for p in held if os.path.basename(p).startswith("."))
starred = 0
for line in open(marks):
    fields = line.rstrip("\n").split("\t")
    if len(fields) == 3 and fields[2] in held:
        starred += fields[1] == str(held[fields[2]] % 5 + 1)
print(named, renamed, hidden, starred)
PY
)
    plans=$(ls -A $XDG_STATE_HOME/glimt/renames 2>/dev/null | wc -l)
    rm -rf $d
    if [ $v = new ]; then
        expect "killed during Rename Many, with photos under hidden names" "$([ $cut -gt 0 ] && echo yes)" yes
        expect "after the new start: each photo under its new name, none hidden, its stars on it, no plan" \
            "$got $plans" "10000 10000 0 10000 0"
    else note "old: named, renamed, hidden, starred, plans: $got $plans ($cut hidden when killed)"; fi
done

section "a keyword save that fails, then Ctrl+Q at once: the failure shown, the window stays open"
# EDGES-1. Closing waits for the sidecar write, and the write then fails: in a read-only folder,
# and on a read-only sidecar. Every sidecar lock is held from outside for a moment, as by another
# window, so the write is still waiting when Ctrl+Q comes.
for v in new old; do
    as_version $v
    for kind in folder sidecar; do
        d=/tmp/close-failed-$v-$kind
        rm -rf $d; mkdir -p $d
        convert -size 300x200 xc:blue $d/a.jpg
        if [ $kind = sidecar ]; then
            printf '<x:xmpmeta xmlns:x="adobe:ns:meta/"/>\n' >$d/a.jpg.xmp
            chmod 444 $d/a.jpg.xmp
        else chmod 555 $d; fi
        before=$(ls -A $d | tr '\n' ' ')$(cat $d/a.jpg.xmp 2>/dev/null)
        printf 'Animals\n' >$XDG_CONFIG_HOME/glimt/keywords
        config info_panel=true
        rm -f /tmp/locks-held
        python3 -c "
import fcntl, os, time
d = os.environ['XDG_RUNTIME_DIR'] + '/glimt/sidecar-locks'
os.makedirs(d, mode=0o700, exist_ok=True)
held = [open(f'{d}/{i}', 'w') for i in range(64)]
for f in held:
    fcntl.flock(f, fcntl.LOCK_EX)
open('/tmp/locks-held', 'w').close()
time.sleep(12)" &
        locker=$!
        until_file /tmp/locks-held
        start $d/a.jpg
        for x in $(seq 1100 4 1240); do xdotool mousemove $x 500; done
        sleep 1
        menu Animals; key ctrl+q
        waits=$(kill -0 $GPID 2>/dev/null && echo waits)
        wait $locker
        sleep 3
        open=$(kill -0 $GPID 2>/dev/null && echo open || echo closed)
        until_notice close-failed-$v-$kind 3; told=$?
        quit
        chmod 755 $d
        after=$(ls -A $d | tr '\n' ' ')$(cat $d/a.jpg.xmp 2>/dev/null)
        got="$waits $open $([ $told = 0 ] && echo told) $([ "$before" = "$after" ] && echo kept)"
        if [ $v = new ]; then
            expect "a read-only $kind, Ctrl+Q while the keyword waits: open, the failure shown, nothing changed" \
                "$got" "waits open told kept"
        else note "old, read-only $kind: $got"; fi
        rm -rf $d
    done
done

section "timings: the large folder and EXIF dates, with nothing else running"
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

    # Work in the window's thread that grows with the list makes frames slow: the median of
    # the window thread's processor time per frame, printed at exit, with the whole list
    # selected and the Delete dialog drawn in every frame. Processor time, not time on the
    # clock: that varied by 20 ms and more from run to run with threads summing file sizes.
    # 0 ms here; a build that made a path of each selected file in every frame took 22 ms.
    config thumbnails=false
    start $big
    wait_first
    key ctrl+a                       # the whole list selected
    key ctrl+d                       # the Delete dialog drawn with it, in every frame
    for i in $(seq 100); do xdotool mousemove $((300 + 3 * i)) 400; sleep 0.05; done
    key Escape
    quit
    median=$(grep -o 'frames: median [0-9]*' /tmp/glimt.err | grep -o '[0-9]*$')
    note "new, 500,000 selected: $(grep -o 'frames: .*' /tmp/glimt.err)"
    expect "frames within 10 ms with 500,000 files selected" \
        "$([ "${median:-9999}" -le 10 ] && echo yes)" yes

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
    rm -rf $e   # 3 GB
fi

section "the folder tree: 100,000 folders, opened at the folder shown, one opened by a click"
d=/tmp/tree
mkdir -p $d/many
(cd $d/many && seq -f 'f%06.0f' 100000 | xargs mkdir -p)
convert -size 64x64 xc:blue $d/many/f000002/b.png
for v in new old; do
    as_version $v
    for tree in false true; do
        config folder_tree=$tree
        start $d/many; sleep 6
        shot tree-$v-$tree
        # Drawn again and again, the pointer moving over it: told, not checked, as drawing the
        # frame in software under Xvfb costs more than the tree does in either version.
        (for i in $(seq 60); do xdotool mousemove 60 $((200 + i % 2 * 40)); sleep 0.05; done) &
        mover=$!
        ticks=$(busy); wait $mover
        eval "ticks_$tree=$ticks"
        if [ $tree = true ]; then
            note "$v: f000002 at $(remote where f000002 | tr -d '\0')"
            menu f000002; sleep 2
            shot tree-$v-clicked
            opened=$(remote current | tr -d '\0')
        fi
        quit
    done
    note "$v: ticks in 3 s with the pointer moving: $ticks_false without the tree, $ticks_true with 100,000 folders in it"
    if [ $v = new ]; then
        expect "the tree's folder opened by a click" "$opened" "$d/many/f000002/b.png"
    fi
done
rm -rf $d

section "as a Flatpak: the host's programs asked of flatpak-spawn, the fallbacks taken on the host"
# A stand-in flatpak-spawn runs the host command here, as the real one does on the host, its
# flags (--host, --env=…) dropped; Glimt takes itself for a Flatpak from FLATPAK_ID. So the
# one place that asks what the host has (host_has_program) and every fallback it decides run here.
cat >/tmp/stub/flatpak-spawn <<'STUB'
#!/bin/sh
while [ "${1#-}" != "$1" ]; do shift; done
exec "$@"
STUB
chmod +x /tmp/stub/flatpak-spawn
d=/tmp/flatpak
mkdir -p $d/kde
convert -size 64x64 xc:red $d/a.jpg
convert -size 64x64 xc:blue $d/kde/k.jpg
# No zenity on the host, kdialog there: the collection dialog (o) goes through kdialog, and its
# answer, a folder, is opened.
rm -f /tmp/stub/zenity
printf '#!/bin/sh\necho "%s"\n' $d/kde >/tmp/stub/kdialog; chmod +x /tmp/stub/kdialog
# The host's screen saver inhibitor: found and started for full screen (looked for in a thread
# since 0.42), and ended when full screen ends.
printf '#!/bin/sh\necho $$ >%s/inhibited\nexec cat\n' $d >/tmp/stub/gnome-session-inhibit
chmod +x /tmp/stub/gnome-session-inhibit
export FLATPAK_ID=io.github.thornbux.Glimt
config
start $d/a.jpg; sleep 2
key o; sleep 3
opened=$(remote current | tr -d '\0')
key f; sleep 2
ticks=$(busy)
key f; sleep 2
ended=$(kill -0 "$(cat $d/inhibited 2>/dev/null)" 2>/dev/null || echo yes)
quit
unset FLATPAK_ID
expect "no zenity on the host: kdialog's answer opened" "$opened" "$d/kde/k.jpg"
expect "the host's inhibitor started, and full screen at rest" "$([ -e $d/inhibited ] && [ $ticks -lt 60 ] && echo yes)" yes
expect "the inhibitor ended with full screen" "$ended" yes

echo "-- not tested here, by hand: the Flatpak (two windows with Ctrl+N, --remote next to the"
echo "   focused one, --remote-all next to both, --remote-all quit)"
if [ $FAILED -gt 0 ]; then echo "$FAILED checks failed"; exit 1; fi
echo "every check passed"
