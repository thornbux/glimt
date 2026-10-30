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

/// A HEIC or AVIF photo, no larger than `max` if given, and then its real size, upright, from
/// the probe. Phones store it as a grid of tiles, which ffmpeg reads as separate streams with
/// their places in a "Tile Grid" group: they are put together with xstack, cut to the image
/// size and turned by the stored rotation.
#[cfg(any(feature = "heic", feature = "avif"))]
pub fn heif(
    path: &Path,
    max: Option<[u32; 2]>,
) -> Result<(image::DynamicImage, Option<[usize; 2]>), String> {
    let mut child = stoppable_command("ffprobe")
        .args(["-v", "error", "-show_stream_groups", "-of", "compact"])
        .args(FILES_ONLY)
        .arg(path)
        .spawn()
        .map_err(|e| format!("HEIC and AVIF need ffprobe (ffmpeg): {e}"))?;
    let (probe, _, _) = output_of(&mut child, 1 << 20, LIMIT);
    let probe = probe.unwrap_or_default();
    let probe = String::from_utf8_lossy(&probe);
    match heif_filter(&probe) {
        Some(filter) => {
            let filter = match fit(max) {
                Some(scale) => filter.replace("[out]", &format!(",{scale}[out]")),
                None => filter,
            };
            let img = ffmpeg_ppm(path, &[], &["-filter_complex", &filter, "-map", "[out]"])?;
            Ok((img, max.and_then(|_| heif_size(&probe))))
        }
        None => Ok((
            ffmpeg_frame(path, "0", max)?,
            max.and_then(|_| probe_size(path)),
        )),
    }
}

/// The size of a tiled HEIF, upright: its "Tile Grid" line's width, height and rotation.
#[cfg(any(feature = "heic", feature = "avif", test))]
fn heif_size(probe: &str) -> Option<[usize; 2]> {
    let grid = probe.lines().find(|l| l.contains("type=Tile Grid"))?;
    let value = |key: &str| {
        (grid.split('|').filter_map(|f| f.split_once('=')))
            .find(|f| f.0.ends_with(key))
            .and_then(|f| f.1.parse::<i64>().ok())
    };
    upright(
        value("tile_grid:width")?,
        value("tile_grid:height")?,
        value("rotation"),
    )
}

/// The size of the picture in `path`, upright, asked of ffprobe: for a frame decoded no larger
/// than the view, which still shows and zooms it at its real size.
#[cfg(any(feature = "video", feature = "heic", feature = "avif"))]
pub fn probe_size(path: &Path) -> Option<[usize; 2]> {
    let mut child = stoppable_command("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=width,height:stream_side_data=rotation")
        .args(["-of", "default=nw=1"])
        .args(FILES_ONLY)
        .arg(path)
        .spawn()
        .ok()?;
    let (out, _, _) = output_of(&mut child, 1 << 16, LIMIT);
    size_in(&String::from_utf8_lossy(&out?))
}

/// The width=, height= and rotation= lines of ffprobe's default output: the size, upright.
#[cfg(any(feature = "video", feature = "heic", feature = "avif", test))]
fn size_in(text: &str) -> Option<[usize; 2]> {
    let value = |key: &str| {
        (text
            .lines()
            .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('=')))
        .and_then(|v| v.trim().parse::<i64>().ok())
    };
    upright(value("width")?, value("height")?, value("rotation"))
}

/// `w` × `h` as shown: swapped for a quarter turn, none for a size that is no size.
#[cfg(any(feature = "video", feature = "heic", feature = "avif", test))]
fn upright(w: i64, h: i64, rotation: Option<i64>) -> Option<[usize; 2]> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    match rotation.unwrap_or(0).rem_euclid(360) {
        90 | 270 => Some([h, w]),
        _ => Some([w, h]),
    }
}

/// Whether ffmpeg decodes `path` (HEIC, AVIF, a video): to the box, with its size probed.
pub fn ffmpeg_decodes(path: &Path) -> bool {
    is_video(path) || is_heif(path)
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

/// `program`'s output on `args`, at most `max` bytes, within `LIMIT`: `None` when it could not
/// run, failed, gave too much or did not end in time. For a question whose answer is all there
/// is (gio info, gio mime, lpstat, gdbus, gsettings).
pub fn limited<S: AsRef<std::ffi::OsStr>>(program: &str, args: &[S], max: u64) -> Option<String> {
    let mut command = stoppable_command(program);
    command.args(args);
    let out = run_limited(command, max, LIMIT).ok()?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// `command` (from `stoppable_command`) run to its end within `limit`, `max` bytes of output at
/// most: its output, or why not (its last error line, or what stopped it). The one way Glimt
/// runs a program for what it says or does and waits for it; clippy refuses `.output()` and
/// `.status()`, which wait for good.
pub fn run_limited(
    mut command: std::process::Command,
    max: u64,
    limit: std::time::Duration,
) -> Result<Vec<u8>, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command.spawn().map_err(|e| format!("{program}: {e}"))?;
    let (out, err, ok) = output_of(&mut child, max, limit);
    let out = out.ok_or_else(|| format!("{program}: too much output"))?;
    match ok {
        true => Ok(out),
        false => Err(match err.lines().last().map(str::trim) {
            Some(last) if !last.is_empty() => last.to_owned(),
            _ => format!("{program} failed or did not end in time"),
        }),
    }
}

/// Move `paths` to the trash with gio, within two minutes: a share that stopped answering left
/// the delete running for good, with no Stop. One call for many files, so thousands do not
/// become thousands of processes.
pub fn trash<S: AsRef<std::ffi::OsStr>>(paths: &[S]) -> Result<(), String> {
    let mut gio = stoppable_command("gio");
    gio.args(["trash", "--"]).args(paths);
    run_limited(gio, 64 << 10, LIMIT * 4).map(drop)
}

/// Ends the process group of `pid` once it has gone `limit` without progress, unless dropped
/// first (the program ended): for a program fed on its input or read line by line (gio mount,
/// lp), which `output_of` cannot run. `feed` tells it of progress (a page taken), so a long
/// print is not ended while the printer takes it. `gave_up` tells, after the wait, whether it
/// was the dog that ended it.
pub struct Watchdog {
    state: std::sync::Arc<Dog>,
}

struct Dog {
    ended: std::sync::atomic::AtomicBool,
    gave_up: std::sync::atomic::AtomicBool,
    fed: std::sync::Mutex<std::time::Instant>,
}

impl Watchdog {
    pub fn new(pid: u32, limit: std::time::Duration) -> Watchdog {
        use std::sync::atomic::Ordering::Relaxed;
        let state = std::sync::Arc::new(Dog {
            ended: false.into(),
            gave_up: false.into(),
            fed: std::time::Instant::now().into(),
        });
        let dog = state.clone();
        std::thread::spawn(move || {
            let step = limit.min(std::time::Duration::from_millis(100));
            loop {
                std::thread::sleep(step);
                if dog.ended.load(Relaxed) {
                    return;
                }
                if dog.fed.lock().unwrap().elapsed() >= limit {
                    dog.gave_up.store(true, Relaxed);
                    stop_group(-(pid as i32), || dog.ended.load(Relaxed));
                    return;
                }
            }
        });
        Watchdog { state }
    }

    /// The program made progress: its `limit` starts again.
    pub fn feed(&self) {
        *self.state.fed.lock().unwrap() = std::time::Instant::now();
    }

    pub fn gave_up(&self) -> bool {
        self.state
            .gave_up
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        (self.state.ended).store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// End the process group `group` (a negative process ID): TERM first, as in a Flatpak
/// flatpak-spawn passes it on to the program on the host, which a KILL of the group cannot
/// reach; then, unless `ended` says within a second that it is over, KILL what is left. The one
/// way Glimt stops a program: the `Watchdog`, which `output_of` runs too, and a program that
/// gave too much.
fn stop_group(group: i32, ended: impl Fn() -> bool) {
    // SAFETY: kill has no preconditions; the group is ours, and not yet waited for.
    unsafe { libc_kill(group, 15) };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while !ended() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !ended() {
        // SAFETY: as above.
        unsafe { libc_kill(group, 9) };
    }
}

/// End the program `pid`, started with `stoppable_command` and not yet waited for, with what it
/// started: Stop for a plugin. Once its group is gone (waited for by its own thread), no KILL
/// follows, which could reach a group that took its number since.
pub fn stop(pid: u32) {
    let group = -(pid as i32);
    // SAFETY: as in stop_group; signal 0 only asks whether the group is there.
    stop_group(group, || unsafe { libc_kill(group, 0) } != 0);
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, signal: i32) -> i32;
}

/// What `child` writes, `max` bytes at most (more: `None`, and it is stopped), its errors, and
/// whether it ended well. Its errors are read beside its output, so neither pipe can fill and
/// stop both. `child` was started with `stoppable_command`; a `Watchdog` stops its group after
/// `limit`.
pub fn output_of(
    child: &mut std::process::Child,
    max: u64,
    limit: std::time::Duration,
) -> (Option<Vec<u8>>, String, bool) {
    use std::io::Read;
    let group = -(child.id() as i32);
    let dog = Watchdog::new(child.id(), limit);
    let errors = child
        .stderr
        .take()
        .map(|e| std::thread::spawn(move || last_text(e, 64 << 10)));
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        #[allow(clippy::disallowed_methods)] // bounded by take
        let _ = stdout.take(max + 1).read_to_end(&mut out);
    }
    let large = out.len() as u64 > max;
    if large {
        stop_group(group, || false);
    }
    #[allow(clippy::disallowed_methods)] // under the Watchdog above
    let ok = child.wait().is_ok_and(|s| s.success());
    // Only once its errors are read to their end: a helper it left behind may hold them open.
    let err = errors.and_then(|t| t.join().ok()).unwrap_or_default();
    drop(dog);
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
    // A frame no larger than Glimt decodes, in `LIMIT`: a crafted file could otherwise keep
    // ffmpeg busy for good, or hand over gigabytes.
    let mut child = stoppable_command("ffmpeg")
        .args(["-v", "error"])
        .args(before)
        .args(FILES_ONLY)
        .arg("-i")
        .arg(path)
        .args(after)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "ppm", "-"])
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
#[allow(clippy::disallowed_methods)] // waits for gio under its Watchdog, answering its prompts
pub fn gio_mount(uri: &str, login: Option<&Login>) -> Mounted {
    use std::io::{Read, Write};
    // An option, not a place: -u or -e would unmount or eject. Not given to gio at all, rather
    // than after a --, which `gio info` reads as a file of its own as well.
    if uri.starts_with('-') {
        return Err(Some(format!("Could not mount {uri}: not an address")));
    }
    // With the usual limit: asked before the mount, this would otherwise wait on a server
    // that never answers, with no watchdog yet.
    let local = || {
        let out = limited("gio", &["info", "-a", "standard::name", "--", uri], 1 << 16)?;
        out.lines()
            .find_map(|l| l.strip_prefix("local path: "))
            .map(PathBuf::from)
    };
    if let Some(path) = local() {
        return Ok(path);
    }
    let mut child = stoppable_command("gio")
        .args(["mount", uri])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Some(format!("Could not run gio: {e}")))?;
    // A server that never answers (a NAS asleep, a VPN down): gio is ended after a minute and
    // the mount told as failed, instead of "Connecting…" and the busy pointer for ever.
    let dog = Watchdog::new(child.id(), LIMIT * 2);
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
    if dog.gave_up() {
        return Err(Some(
            crate::lang::tr("No answer from the server in a minute").into(),
        ));
    }
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

/// `uri` without a password written into it (smb://user:secret@nas/share), and that password,
/// percent-decoded: given to gio as a URI, it would be on gio's command line, which every user
/// of the computer can read in the process list, and on the screen in "Connecting to …".
pub fn without_password(uri: &str) -> (String, Option<String>) {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return (uri.to_owned(), None);
    };
    let end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let Some((info, host)) = authority.rsplit_once('@') else {
        return (uri.to_owned(), None);
    };
    let Some((user, password)) = info.split_once(':') else {
        return (uri.to_owned(), None);
    };
    let password = crate::collections::percent_decoded(password.as_bytes());
    let password = String::from_utf8_lossy(&password).into_owned();
    let user = match user.is_empty() {
        true => String::new(),
        false => format!("{user}@"),
    };
    // An empty one (smb://me:@nas) is none: the login dialog asks for it, as without it.
    let password = (!password.is_empty()).then_some(password);
    (format!("{scheme}://{user}{host}{path}"), password)
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
    // With the usual limit: a file on a share that went away would else hold the menu at "…".
    let gio = |args: &[&std::ffi::OsStr]| limited("gio", args, 1 << 16).unwrap_or_default();
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
    named(stoppable_command("sh"), &ids.collect::<Vec<_>>())
}

/// The names and desktop files of the applications `ids`, found by `sh`: on the host in a
/// Flatpak, where the desktop files are and where `gio launch` runs, not in the sandbox.
fn named(mut sh: std::process::Command, ids: &[&str]) -> Vec<(String, PathBuf)> {
    // The "applications" folder of each XDG data folder, the first one with the file.
    // Split at the colons as a variable's value: dash does not split at one written between two.
    const FIND: &str = r#"dirs="${XDG_DATA_HOME:-$HOME/.local/share}:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
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
    sh.arg("-c").arg(FIND).arg("sh").args(ids);
    let text = run_limited(sh, 1 << 20, LIMIT).map(|o| String::from_utf8_lossy(&o).into_owned());
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
        #[allow(clippy::disallowed_methods)]
        // the browser may stay open: waited for in its own thread
        let run = |cmd: &str, args: &[&str]| command(cmd).args(args).arg(&url).status();
        if !run("gio", &["open"]).is_ok_and(|s| s.success()) {
            let _ = run("xdg-open", &[]);
        }
    });
}

/// File → Set as Wallpaper: `path` as the desktop's background, through the desktop portal,
/// which GNOME and KDE have, also for a Flatpak. GNOME shows it and asks first; both keep a copy
/// of their own, so the photo may be moved or deleted later. gdbus comes with gio.
pub fn set_wallpaper(path: &Path) -> Result<(), String> {
    // A GVariant string: uri_path leaves no " or \ in it.
    let uri = format!("\"file://{}\"", crate::thumbs::uri_path(path));
    let mut gdbus = stoppable_command("gdbus");
    gdbus
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
        ])
        .args([
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
        ])
        .arg("org.freedesktop.portal.Wallpaper.SetWallpaperURI")
        .args([
            "",
            &uri,
            "{'show-preview': <true>, 'set-on': <'background'>}",
        ]);
    run_limited(gdbus, 1 << 16, LIMIT).map(drop)
}

/// An external program: ffmpeg, gio, zenity, a player. Inside a Flatpak it runs on the host,
/// which has them, through `flatpak-spawn --host`.
pub fn command(program: &str) -> std::process::Command {
    host_command(program, &[])
}

/// As `command`, for a program Glimt may have to stop, with what every such program needs: a
/// process group of its own, so its children are stopped with it; no terminal for its input, as
/// one started from a shell would stop itself (SIGTTOU) touching the terminal from the
/// background; and its output and errors piped, for `output_of`. One fed on its input sets
/// `.stdin(Stdio::piped())` after. Inside a Flatpak it runs on the host, out of reach of a
/// signal to its process group, and `--watch-bus` ends it when its flatpak-spawn is killed.
pub fn stoppable_command(program: &str) -> std::process::Command {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut command = host_command(program, &["--watch-bus"]);
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Whether `program` is there to run: on the host in a Flatpak, where flatpak-spawn starts
/// even for one the host lacks and fails only afterwards. Asked of the shell and remembered
/// once it is there; one that is not is asked for again, so a program installed while Glimt
/// runs is found without a restart. The one way every fallback decides: zenity or kdialog, gio
/// or xdg-open, which screen saver inhibitor, sqlite3.
pub fn host_has(program: &str) -> bool {
    static KNOWN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> =
        std::sync::OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(&has) = known.lock().unwrap().get(program) {
        return has;
    }
    let mut sh = stoppable_command("sh");
    sh.args(["-c", "command -v -- \"$1\" >/dev/null 2>&1", "sh", program]);
    let has = run_limited(sh, 1 << 10, LIMIT).is_ok();
    if has {
        known.lock().unwrap().insert(program.to_owned(), true);
    }
    has
}

#[allow(clippy::disallowed_methods)] // the one place a program is named to run
fn host_command(program: &str, flags: &[&str]) -> std::process::Command {
    if std::env::var_os("FLATPAK_ID").is_none() {
        return std::process::Command::new(program);
    }
    let mut cmd = std::process::Command::new("flatpak-spawn");
    let home = std::env::var("HOME").unwrap_or_default();
    (cmd.arg("--host").args(flags))
        .args(host_data_dirs(&home))
        .arg(program);
    cmd
}

/// flatpak-spawn passes the sandbox's variables on to the host: its data folder, /app/share.
/// The host's own data folders instead, with those Flatpak exports installed programs to, so
/// that gio lists them for Open With and `named` finds their desktop files.
fn host_data_dirs(home: &str) -> [String; 2] {
    [
        format!("--env=XDG_DATA_HOME={home}/.local/share"),
        format!(
            "--env=XDG_DATA_DIRS=/usr/local/share:/usr/share:/var/lib/flatpak/exports/share:{home}/.local/share/flatpak/exports/share"
        ),
    ]
}

#[cfg(test)]
mod tests {
    /// A program that is there is remembered; one that is not is asked for again, so it is
    /// found once installed.
    #[test]
    fn host_has_remembers_only_what_is_there() {
        assert!(super::host_has("sh") && super::host_has("sh"));
        assert!(!super::host_has("glimt-no-such-program-xyz"));
        assert!(!super::host_has("glimt-no-such-program-xyz"));
    }

    /// The dog ends a program that does not: sleep 30 is over in a tenth of a second.
    #[test]
    fn the_watchdog_ends_a_program_in_time() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let dog = super::Watchdog::new(child.id(), std::time::Duration::from_millis(100));
        let start = std::time::Instant::now();
        assert!(!child.wait().unwrap().success());
        assert!(dog.gave_up() && start.elapsed() < std::time::Duration::from_secs(5));
        // One that ends in time is left alone, and the dog says so.
        let mut quick = std::process::Command::new("true")
            .process_group(0)
            .spawn()
            .unwrap();
        let dog = super::Watchdog::new(quick.id(), std::time::Duration::from_secs(10));
        assert!(quick.wait().unwrap().success() && !dog.gave_up());
    }

    /// One that keeps making progress is not ended at the limit: a print to lp longer than two
    /// minutes, the PDF made while lp takes it, was ended halfway. Unfed, it is ended.
    #[test]
    fn the_watchdog_waits_while_fed() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let dog = super::Watchdog::new(child.id(), std::time::Duration::from_millis(300));
        // A second, three times the limit, fed every tenth.
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            dog.feed();
        }
        assert!(!dog.gave_up());
        let start = std::time::Instant::now();
        assert!(!child.wait().unwrap().success());
        assert!(dog.gave_up() && start.elapsed() < std::time::Duration::from_secs(5));
    }

    /// Every program Glimt may stop has a process group of its own and no terminal for its
    /// input: ffmpeg, handed the terminal Glimt was started from, stopped itself (SIGTTOU) and
    /// every HEIC, AVIF and video frame waited 30 seconds for nothing.
    #[test]
    fn a_stoppable_program_has_its_own_group_and_no_terminal() {
        let script = "readlink /proc/self/fd/0; set -- $(cat /proc/$$/stat); [ \"$1\" = \"$5\" ] && echo own";
        let mut child = super::stoppable_command("sh")
            .args(["-c", script])
            .spawn()
            .unwrap();
        let (out, _, ok) = super::output_of(&mut child, 1024, super::LIMIT);
        assert!(ok);
        assert_eq!(String::from_utf8_lossy(&out.unwrap()), "/dev/null\nown\n");
    }

    /// A program run for its answer is ended at its limit and says why: `gio trash` on a share
    /// that stopped answering held a delete for good, gdbus and the shell's lookups as well.
    #[test]
    fn run_limited_ends_at_its_limit() {
        let mut sleep = super::stoppable_command("sleep");
        sleep.arg("30");
        let start = std::time::Instant::now();
        let ended = super::run_limited(sleep, 1024, std::time::Duration::from_millis(200));
        assert!(ended.is_err() && start.elapsed() < std::time::Duration::from_secs(5));
        let mut echo = super::stoppable_command("sh");
        echo.args(["-c", "echo yes; echo why >&2"]);
        assert_eq!(
            super::run_limited(echo, 1024, super::LIMIT).unwrap(),
            b"yes\n"
        );
        let mut fails = super::stoppable_command("sh");
        fails.args(["-c", "echo first >&2; echo the reason >&2; exit 1"]);
        assert_eq!(
            super::run_limited(fails, 1024, super::LIMIT).unwrap_err(),
            "the reason"
        );
    }

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
        let mut sh = super::stoppable_command("sh");
        sh.env("XDG_DATA_HOME", dir.join("a"))
            .env("XDG_DATA_DIRS", dir.join("b"));
        let found = super::named(sh, &["x.desktop", "y.desktop", "none.desktop"]);
        assert_eq!(
            found,
            [
                ("Ex".to_owned(), dir.join("a/applications/x.desktop")),
                ("Why".to_owned(), dir.join("b/applications/y.desktop"))
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// In a Flatpak the host's programs get the host's data folders, not the sandbox's.
    #[test]
    fn host_gets_its_own_data_dirs() {
        let [home, dirs] = super::host_data_dirs("/home/me");
        assert_eq!(home, "--env=XDG_DATA_HOME=/home/me/.local/share");
        assert!(dirs.starts_with("--env=XDG_DATA_DIRS=/usr/local/share:/usr/share:"));
        assert!(dirs.ends_with(":/home/me/.local/share/flatpak/exports/share"));
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

    /// An address that begins with - is no address: refused before gio sees it, also where no
    /// gio is installed (issue #7).
    #[test]
    fn refuses_an_address_that_is_an_option() {
        let refused = super::gio_mount("-u", None).unwrap_err().unwrap();
        assert!(refused.ends_with("not an address"), "{refused}");
    }

    /// A password in a URI is taken out of it, decoded, and the user name kept; a URI without
    /// one is as it was.
    #[test]
    fn takes_the_password_out_of_a_uri() {
        let (uri, password) = super::without_password("smb://me:p%40ss:w@nas:445/share/a@b");
        assert_eq!(uri, "smb://me@nas:445/share/a@b");
        assert_eq!(password.as_deref(), Some("p@ss:w"));
        // An empty one is none: asked for in the login dialog, as without it.
        assert_eq!(super::without_password("smb://me:@nas/share").1, None);
        let (uri, password) = super::without_password("sftp://:secret@host/home");
        assert_eq!(
            (uri.as_str(), password.as_deref()),
            ("sftp://host/home", Some("secret"))
        );
        for plain in [
            "smb://me@nas/share",
            "smb://nas/a:b@c",
            "/home/me",
            "smb://nas",
        ] {
            assert_eq!(super::without_password(plain), (plain.to_owned(), None));
        }
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

    /// The real size of a frame decoded to the box: a tiled HEIF's from its grid line, a
    /// video's from the stream, both turned upright by the stored rotation.
    #[test]
    fn sizes_from_the_probe() {
        let grid = "stream_group|index=0|type=Tile Grid|c:tile_grid:width=1000|\
            c:tile_grid:height=500|c:rotation=-90|t:stream_index=0\n";
        assert_eq!(super::heif_size(grid), Some([500, 1000]));
        assert_eq!(super::heif_size("stream|index=0\n"), None);
        assert_eq!(
            super::size_in("width=1920\nheight=1080\n"),
            Some([1920, 1080])
        );
        assert_eq!(
            super::size_in("width=1920\nheight=1080\nrotation=-90\n"),
            Some([1080, 1920])
        );
        assert_eq!(super::size_in("width=0\nheight=1080\n"), None);
        assert_eq!(super::size_in("height=1080\n"), None);
    }

    /// ffprobe output broken in thousands of ways: no panic, and only numbers in the filter.
    #[test]
    fn survives_broken_probes() {
        let probe = "stream_group|index=0|type=Tile Grid|c:tile_grid:width=1000|\
            c:tile_grid:height=500|c:rotation=-90|t:stream_index=0|t:tile_horizontal_offset=0|\
            t:tile_vertical_offset=0|t:stream_index=1|t:tile_horizontal_offset=512|\
            t:tile_vertical_offset=0\n";
        for bytes in crate::broken(probe.as_bytes()) {
            let text = String::from_utf8_lossy(&bytes);
            if let Some(filter) = super::heif_filter(&text) {
                assert!(!filter.contains(';'), "{filter}");
            }
            super::heif_size(&text);
        }
        for bytes in crate::broken(b"width=1920\nheight=1080\nrotation=-90\n") {
            super::size_in(&String::from_utf8_lossy(&bytes));
        }
    }
}
