//! External programs: ffmpeg for HEIC, AVIF and video frames, gio for mounting and the programs
//! that open a file, xdg-open; inside a Flatpak they run on the host (`command`). `output_of`
//! reads a program's output with a limit of time and size, also for decoder plugins, and
//! `stoppable_command` starts one that can be stopped with everything it started.

use crate::files::{AVIF_EXTS, HEIC_EXTS, has_ext};
use std::path::{Path, PathBuf};

/// HEIC and AVIF: both HEIF files, decoded the same way.
pub fn is_heif(path: &Path) -> bool {
    has_ext(path, HEIC_EXTS) || has_ext(path, AVIF_EXTS)
}

/// A frame `at` seconds in, decoded by the `ffmpeg` program; no larger than `max` if given.
#[cfg(any(feature = "video", feature = "heic", feature = "avif"))]
pub fn ffmpeg_frame(
    path: &Path,
    at: &str,
    max: Option<[u32; 2]>,
) -> Result<image::DynamicImage, String> {
    match fit(max) {
        Some(scale) => ffmpeg_ppm(path, &["-ss", at], &["-vf", &scale]),
        None => ffmpeg_ppm(path, &["-ss", at], &[]),
    }
}

/// An ffmpeg filter that shrinks to fit `max`, keeping the shape, and never enlarges: a
/// thumbnail of a 48 MP phone photo or an 8K video frame needs no 150 MB picture first.
#[cfg(any(feature = "video", feature = "heic", feature = "avif", test))]
fn fit(max: Option<[u32; 2]>) -> Option<String> {
    let [w, h] = max?;
    Some(format!(
        "scale='min({w},iw)':'min({h},ih)':force_original_aspect_ratio=decrease"
    ))
}

/// A HEIC or AVIF photo, no larger than `max` if given. Phones store it as a grid of tiles,
/// which ffmpeg reads as separate streams with their places in a "Tile Grid" group: they are
/// put together with xstack, cut to the image size and turned by the stored rotation.
#[cfg(any(feature = "heic", feature = "avif"))]
pub fn heif(path: &Path, max: Option<[u32; 2]>) -> Result<image::DynamicImage, String> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut child = stoppable_command("ffprobe")
        .process_group(0)
        .args(["-v", "error", "-show_stream_groups", "-of", "compact"])
        .args(FILES_ONLY)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("HEIC and AVIF need ffprobe (ffmpeg): {e}"))?;
    let (probe, _, _) = output_of(&mut child, 1 << 20, LIMIT);
    match heif_filter(&String::from_utf8_lossy(&probe.unwrap_or_default())) {
        Some(filter) => {
            let filter = match fit(max) {
                Some(scale) => filter.replace("[out]", &format!(",{scale}[out]")),
                None => filter,
            };
            ffmpeg_ppm(path, &[], &["-filter_complex", &filter, "-map", "[out]"])
        }
        None => ffmpeg_frame(path, "0", max),
    }
}

/// The ffmpeg filter that puts the tiles of the "Tile Grid" in ffprobe's compact output
/// together; `None` for an image that is not split into tiles.
#[cfg(any(feature = "heic", feature = "avif", test))]
pub fn heif_filter(probe: &str) -> Option<String> {
    let grid = probe.lines().find(|l| l.contains("type=Tile Grid"))?;
    // Numbers only: the values go into ffmpeg's filter text, where other text could add filters.
    let fields: Vec<(&str, &str)> = (grid.split('|'))
        .filter_map(|f| f.split_once('='))
        .filter(|f| f.1.parse::<i64>().is_ok())
        .collect();
    let get = |key: &str| {
        fields
            .iter()
            .find(|f| f.0.ends_with(key))
            .map_or("0", |f| f.1)
    };
    let (mut inputs, mut layout) = (String::new(), Vec::<String>::new());
    for (key, value) in &fields {
        if key.ends_with(":stream_index") {
            inputs += &format!("[0:{value}]");
        } else if key.ends_with(":tile_horizontal_offset") {
            layout.push(value.to_string());
        } else if key.ends_with(":tile_vertical_offset")
            && let Some(x) = layout.last_mut()
        {
            *x += &format!("_{value}");
        }
    }
    if layout.len() < 2 {
        return None;
    }
    let turn = match get("rotation") {
        "-90" | "270" => ",transpose=clock",
        "90" | "-270" => ",transpose=cclock",
        "180" | "-180" => ",hflip,vflip",
        _ => "",
    };
    Some(format!(
        "{inputs}xstack=inputs={}:layout={},crop={}:{}:{}:{}{turn}[out]",
        layout.len(),
        layout.join("|"),
        get("tile_grid:width"),
        get("tile_grid:height"),
        get("tile_grid:horizontal_offset"),
        get("tile_grid:vertical_offset"),
    ))
}

/// ffmpeg's input options for files that come from anywhere: only local files (no network),
/// and only the formats of photos and videos Glimt lists. ffmpeg tells the format by the
/// content, so a "video.mp4" that is a playlist (HLS, concat) would else open the files or URLs
/// it names, for a preview made without the file even being opened.
#[cfg(any(feature = "video", feature = "heic", feature = "avif"))]
const FILES_ONLY: [&str; 4] = [
    "-protocol_whitelist",
    "file",
    "-format_whitelist",
    // HEIC, AVIF, MP4, MOV, 3GP; MKV, WebM; AVI; MPEG; TS, MTS, M2TS; WMV; FLV.
    "mov,matroska,avi,mpeg,mpegts,asf,flv",
];

/// How long a program may take over one file: a crafted file, or a share that stops answering,
/// can keep it busy for good, and the decoder thread with it.
pub const LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// What `child` writes, `max` bytes at most (more: `None`, and it is stopped), its errors, and
/// whether it ended well. Its errors are read beside its output, so neither pipe can fill and
/// stop both. `child` was started with `stoppable_command` in a process group of its own, which
/// is stopped after `limit`: TERM first, as in a Flatpak flatpak-spawn passes it on to the
/// program on the host, which a KILL of the group cannot reach; then KILL what is left.
pub fn output_of(
    child: &mut std::process::Child,
    max: u64,
    limit: std::time::Duration,
) -> (Option<Vec<u8>>, String, bool) {
    use crate::decoders::libc_kill;
    use std::io::Read;
    let group = -(child.id() as i32);
    let (done, timer) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if timer.recv_timeout(limit).is_err() {
            // SAFETY: kill has no preconditions; the group is ours, and not yet waited for.
            unsafe { libc_kill(group, 15) };
            if timer
                .recv_timeout(std::time::Duration::from_secs(1))
                .is_err()
            {
                // SAFETY: as above.
                unsafe { libc_kill(group, 9) };
            }
        }
    });
    let errors = child
        .stderr
        .take()
        .map(|e| std::thread::spawn(move || last_text(e, 64 << 10)));
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(max + 1).read_to_end(&mut out);
    }
    let large = out.len() as u64 > max;
    if large {
        // SAFETY: as above.
        unsafe { libc_kill(group, 9) };
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    // Only once its errors are read to their end: a helper it left behind may hold them open.
    let err = errors.and_then(|t| t.join().ok()).unwrap_or_default();
    let _ = done.send(());
    ((!large).then_some(out), err, ok)
}

/// The last `keep` bytes or so that `from` writes, read to its end so the program never waits on
/// a full pipe, as text even where it is not UTF-8 (a file name in another encoding): a damaged
/// video can make ffmpeg write an error for every packet, and the last line is what is told.
fn last_text(mut from: impl std::io::Read, keep: usize) -> String {
    let (mut text, mut chunk) = (Vec::new(), [0u8; 8192]);
    while let Ok(n @ 1..) = from.read(&mut chunk) {
        text.extend_from_slice(&chunk[..n]);
        if text.len() > 2 * keep {
            text.drain(..text.len() - keep);
        }
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// One frame of `path` as decoded by ffmpeg, with options before and after the input.
#[cfg(any(feature = "video", feature = "heic", feature = "avif"))]
pub fn ffmpeg_ppm(
    path: &Path,
    before: &[&str],
    after: &[&str],
) -> Result<image::DynamicImage, String> {
    use std::process::Stdio;
    // A frame no larger than Glimt decodes, in `LIMIT`: a crafted file could otherwise keep
    // ffmpeg busy for good, or hand over gigabytes.
    use std::os::unix::process::CommandExt;
    let mut child = stoppable_command("ffmpeg")
        .process_group(0)
        .args(["-v", "error"])
        .args(before)
        .args(FILES_ONLY)
        .arg("-i")
        .arg(path)
        .args(after)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "ppm", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("video, HEIC and AVIF need ffmpeg: {e}"))?;
    let limit = crate::decode::MAX_DECODE + 64;
    let (frame, err, ok) = output_of(&mut child, limit, LIMIT);
    let frame = frame.ok_or("image too large")?;
    if !ok || frame.is_empty() {
        let err = err.trim();
        return Err(if err.is_empty() {
            "no frame".into()
        } else {
            err.to_owned()
        });
    }
    image::load_from_memory_with_format(&frame, image::ImageFormat::Pnm).map_err(|e| e.to_string())
}

#[cfg(feature = "video")]
pub fn is_video(path: &Path) -> bool {
    has_ext(path, crate::video::EXTS)
}

#[cfg(not(feature = "video"))]
pub fn is_video(_: &Path) -> bool {
    false
}

/// A mounted share's local path; `Err(None)` = a login is needed, `Err(Some)` = why it failed.
pub type Mounted = Result<PathBuf, Option<String>>;

/// User name, domain and password for a network share.
#[derive(Clone, Default)]
pub struct Login {
    pub uri: String,
    pub user: String,
    pub domain: String,
    pub password: String,
}

/// The local path of a URI (smb://, sftp:// …), mounting it with gio first if needed: GVfs
/// puts mounted shares under /run/user/<uid>/gvfs. gio asks for what the share needs (user,
/// domain, password) one prompt at a time; they are answered from `login`, or, without one,
/// `Err(None)` says that a login is needed. A password saved in the keyring (from Files) is
/// used without asking.
pub fn gio_mount(uri: &str, login: Option<&Login>) -> Mounted {
    use std::io::{Read, Write};
    let local = || {
        let out = command("gio")
            .args(["info", "-a", "standard::name", uri])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("local path: "))
            .map(PathBuf::from)
    };
    if let Some(path) = local() {
        return Ok(path);
    }
    let mut child = command("gio")
        .args(["mount", uri])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Some(format!("Could not run gio: {e}")))?;
    let (mut out, mut input) = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    // Prompts end in ": " without a newline, the next one printed after the answer.
    let (mut text, mut answered, mut buf) = (String::new(), 0, [0; 512]);
    // gio asks again after a wrong password; answering again and again could lock the account.
    let mut passwords = 0;
    while let Ok(n @ 1..) = out.read(&mut buf) {
        text.push_str(&String::from_utf8_lossy(&buf[..n]));
        let prompt = &text[answered..];
        if !prompt.ends_with(": ") {
            continue;
        }
        let Some(login) = login else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(None);
        };
        let prompt = prompt.rsplit('\n').next().unwrap_or(prompt).to_lowercase();
        let answer = if prompt.contains("password") {
            passwords += 1;
            if passwords > 1 {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Some(format!(
                    "Could not mount {uri}: wrong user name or password"
                )));
            }
            &login.password
        } else if prompt.contains("domain") {
            &login.domain
        } else if prompt.contains("user") {
            &login.user
        } else {
            // "Login anonymously?" and anything else: the default.
            ""
        };
        let _ = writeln!(input, "{answer}");
        answered = text.len();
    }
    let out = child.wait_with_output().map_err(|e| Some(e.to_string()))?;
    local().ok_or_else(|| {
        let err = String::from_utf8_lossy(&out.stderr);
        // "gio: smb://server/share/: Failed to mount …": the reason only.
        let err = err.lines().last().map_or("", str::trim);
        let err = err
            .strip_prefix("gio: ")
            .and_then(|e| e.split_once(": "))
            .map_or(err, |e| e.1);
        Some(format!("Could not mount {uri}: {err}{}", unknown_host(uri)))
    })
}

/// A hint when the server's name in `uri` is not found by the system's name lookup: a NAS
/// is often known only as name.local (mDNS) or by its address, and Samba then fails with
/// "Invalid argument" after the login rather than saying so.
fn unknown_host(uri: &str) -> String {
    use std::net::ToSocketAddrs;
    let host = uri_host(uri);
    if host.is_empty()
        || (host, 445)
            .to_socket_addrs()
            .is_ok_and(|mut a| a.next().is_some())
    {
        return String::new();
    }
    match host.contains('.') {
        true => format!(" (the name {host} is not found)"),
        false => format!(" (the name {host} is not found; try {host}.local or its IP address)"),
    }
}

/// The server's name in `uri`: after the user and password (which may hold a colon), before the
/// port; empty for an IPv6 address in brackets, which is no name to look up.
fn uri_host(uri: &str) -> &str {
    let rest = uri.split_once("://").map_or("", |r| r.1);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or(authority);
    match host.starts_with('[') {
        true => "",
        false => host.split(':').next().unwrap_or_default(),
    }
}

/// Applications registered for the type of `path` (gio mime), as (name, desktop file), Glimt
/// itself left out.
pub fn apps_for(path: &Path) -> Vec<(String, PathBuf)> {
    let gio = |args: &[&std::ffi::OsStr]| {
        command("gio")
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let info = gio(&[
        "info".as_ref(),
        "-a".as_ref(),
        "standard::content-type".as_ref(),
        path.as_os_str(),
    ]);
    let Some(mime) = info
        .lines()
        .find_map(|l| l.trim().strip_prefix("standard::content-type: "))
    else {
        return Vec::new();
    };
    let apps = gio(&["mime".as_ref(), mime.as_ref()]);
    let ids = apps
        .lines()
        .skip_while(|l| !l.starts_with("Registered applications"))
        .skip(1)
        .take_while(|l| l.starts_with('\t'))
        .map(str::trim)
        // Glimt itself, as a package or as the Flatpak.
        .filter(|id| !matches!(*id, "glimt.desktop" | "io.github.thornbux.Glimt.desktop"));
    named(command("sh"), &ids.collect::<Vec<_>>())
}

/// The names and desktop files of the applications `ids`, found by `sh`: on the host in a
/// Flatpak, where the desktop files are and where `gio launch` runs, not in the sandbox.
fn named(mut sh: std::process::Command, ids: &[&str]) -> Vec<(String, PathBuf)> {
    // The "applications" folder of each XDG data folder, the first one with the file. In a
    // Flatpak the host gets the sandbox's variables (its data folder, /app/share): the host's
    // own folders then, with those Flatpak exports installed programs to.
    // Split at the colons as a variable's value: dash does not split at one written between two.
    const FIND: &str = r#"dirs="${XDG_DATA_HOME:-$HOME/.local/share}:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
    if [ -n "$FLATPAK_ID" ]; then
        dirs="$HOME/.local/share:/usr/local/share:/usr/share:/var/lib/flatpak/exports/share:$HOME/.local/share/flatpak/exports/share"
    fi
    for id in "$@"; do
        IFS=:
        for d in $dirs; do
            f="$d/applications/$id"
            if [ -f "$f" ]; then
                n=$(grep -m1 '^Name=' "$f")
                printf '%s\t%s\n' "${n#Name=}" "$f"
                break
            fi
        done
    done"#;
    let out = sh.arg("-c").arg(FIND).arg("sh").args(ids).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    (text.unwrap_or_default().lines())
        .filter_map(|l| l.split_once('\t'))
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, file)| (name.to_owned(), PathBuf::from(file)))
        .collect()
}

/// Open `url` in the default browser, in the background.
pub fn open_url(url: &str) {
    let url = url.to_owned();
    std::thread::spawn(move || {
        let run = |cmd: &str, args: &[&str]| command(cmd).args(args).arg(&url).status();
        if !run("gio", &["open"]).is_ok_and(|s| s.success()) {
            let _ = run("xdg-open", &[]);
        }
    });
}

/// An external program: ffmpeg, gio, zenity, a player. Inside a Flatpak it runs on the host,
/// which has them, through `flatpak-spawn --host`.
pub fn command(program: &str) -> std::process::Command {
    host_command(program, &[])
}

/// As `command`, for a program Glimt may have to stop: inside a Flatpak it runs on the host,
/// out of reach of a signal to its process group, and `--watch-bus` ends it when its
/// flatpak-spawn is killed.
pub fn stoppable_command(program: &str) -> std::process::Command {
    host_command(program, &["--watch-bus"])
}

/// Whether `program` is there to run: on the host in a Flatpak, where flatpak-spawn starts
/// even for one the host lacks and fails only afterwards. Asked of the shell once for each
/// program and remembered. The one way every fallback decides: zenity or kdialog, gio or
/// xdg-open, which screen saver inhibitor, sqlite3.
pub fn host_has(program: &str) -> bool {
    static KNOWN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> =
        std::sync::OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(&has) = known.lock().unwrap().get(program) {
        return has;
    }
    let has = command("sh")
        .args(["-c", "command -v -- \"$1\" >/dev/null 2>&1", "sh", program])
        .status()
        .is_ok_and(|s| s.success());
    known.lock().unwrap().insert(program.to_owned(), has);
    has
}

fn host_command(program: &str, flags: &[&str]) -> std::process::Command {
    if std::env::var_os("FLATPAK_ID").is_none() {
        return std::process::Command::new(program);
    }
    let mut cmd = std::process::Command::new("flatpak-spawn");
    cmd.arg("--host").args(flags).arg(program);
    cmd
}

#[cfg(test)]
mod tests {
    /// A program is there or not, and the shell is asked once.
    #[test]
    fn knows_what_the_host_has() {
        assert!(super::host_has("sh"));
        assert!(!super::host_has("no-such-program-glimt"));
        assert!(!super::host_has("no-such-program-glimt"));
    }

    /// Desktop files found in the first data folder that has them, with their names.
    #[test]
    fn names_applications() {
        let dir = std::env::temp_dir().join(format!("glimt-test-apps-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/applications")).unwrap();
        std::fs::create_dir_all(dir.join("b/applications")).unwrap();
        std::fs::write(
            dir.join("a/applications/x.desktop"),
            "[Desktop Entry]\nName=Ex\n",
        )
        .unwrap();
        std::fs::write(dir.join("b/applications/x.desktop"), "Name=Other\n").unwrap();
        std::fs::write(dir.join("b/applications/y.desktop"), "Name=Why\n").unwrap();
        let mut sh = std::process::Command::new("sh");
        sh.env("XDG_DATA_HOME", dir.join("a"))
            .env("XDG_DATA_DIRS", dir.join("b"))
            .env_remove("FLATPAK_ID");
        let found = super::named(sh, &["x.desktop", "y.desktop", "none.desktop"]);
        assert_eq!(
            found,
            [
                ("Ex".to_owned(), dir.join("a/applications/x.desktop")),
                ("Why".to_owned(), dir.join("b/applications/y.desktop"))
            ]
        );
        // As in a Flatpak: the sandbox's variables are passed on, so the host's folders are
        // looked in instead, among them the one of a user's own programs.
        std::fs::create_dir_all(dir.join("home/.local/share/applications")).unwrap();
        std::fs::write(
            dir.join("home/.local/share/applications/z.desktop"),
            "Name=Zed\n",
        )
        .unwrap();
        let mut sh = std::process::Command::new("sh");
        sh.env("XDG_DATA_HOME", dir.join("a"))
            .env("XDG_DATA_DIRS", dir.join("b"))
            .env("HOME", dir.join("home"))
            .env("FLATPAK_ID", "io.github.thornbux.Glimt");
        let found = super::named(sh, &["x.desktop", "z.desktop"]);
        assert_eq!(
            found,
            [(
                "Zed".to_owned(),
                dir.join("home/.local/share/applications/z.desktop")
            )]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A program's errors: kept to their last part, however much it writes, and told even in
    /// another encoding.
    #[test]
    fn keeps_the_last_errors() {
        let mut out = vec![b'x'; 1 << 20];
        out.extend(b"\nferie_\xe6.mp4: no frame");
        let text = super::last_text(&out[..], 1000);
        assert!(text.len() <= 2000);
        assert!(text.ends_with(".mp4: no frame"));
    }

    /// A server name the system finds needs no hint; one it does not gets name.local or the IP.
    /// The name is the one after a user and password and before a port.
    #[test]
    fn hints_at_an_unknown_host() {
        assert_eq!(super::unknown_host("smb://localhost/share"), "");
        assert_eq!(super::uri_host("smb://user:pw@nas:445/share"), "nas");
        assert_eq!(super::uri_host("sftp://[fe80::1]/home"), "");
        assert_eq!(super::uri_host("smb://nas.local"), "nas.local");
        assert_eq!(super::unknown_host("smb://user@127.0.0.1/share"), "");
        let hint = super::unknown_host("smb://no-such-glimt-host/share");
        assert!(hint.contains("no-such-glimt-host.local"), "{hint}");
    }

    /// A thumbnail's size as ffmpeg's filter: shrunk to fit, never enlarged; none for the view.
    #[test]
    fn fits_ffmpeg_frames() {
        assert_eq!(
            super::fit(Some([256, 256])).unwrap(),
            "scale='min(256,iw)':'min(256,ih)':force_original_aspect_ratio=decrease"
        );
        assert_eq!(super::fit(None), None);
    }

    /// Two tiles side by side, cut to 1000×500 and turned a quarter: the keys as the parser
    /// reads them from ffprobe's compact output.
    #[test]
    fn heif_grid_filter() {
        let probe = "stream_group|index=0|type=Tile Grid|c:tile_grid:width=1000|\
            c:tile_grid:height=500|c:tile_grid:horizontal_offset=0|\
            c:tile_grid:vertical_offset=0|c:rotation=-90|\
            t:stream_index=0|t:tile_horizontal_offset=0|t:tile_vertical_offset=0|\
            t:stream_index=1|t:tile_horizontal_offset=512|t:tile_vertical_offset=0\n";
        assert_eq!(
            super::heif_filter(probe).as_deref(),
            Some(
                "[0:0][0:1]xstack=inputs=2:layout=0_0|512_0,crop=1000:500:0:0,\
                 transpose=clock[out]"
            )
        );
        assert_eq!(super::heif_filter("stream|index=0\n"), None);
        // A value that is not a number is left out, and adds no filter of its own.
        let hostile = probe.replace("t:stream_index=1", "t:stream_index=1];movie=x[a");
        assert!(!super::heif_filter(&hostile).unwrap().contains("movie"));
    }

    /// ffprobe output broken in thousands of ways: no panic, and only numbers in the filter.
    #[test]
    fn survives_broken_probes() {
        let probe = "stream_group|index=0|type=Tile Grid|c:tile_grid:width=1000|\
            c:tile_grid:height=500|c:rotation=-90|t:stream_index=0|t:tile_horizontal_offset=0|\
            t:tile_vertical_offset=0|t:stream_index=1|t:tile_horizontal_offset=512|\
            t:tile_vertical_offset=0\n";
        for bytes in crate::broken(probe.as_bytes()) {
            if let Some(filter) = super::heif_filter(&String::from_utf8_lossy(&bytes)) {
                assert!(!filter.contains(';'), "{filter}");
            }
        }
    }
}
