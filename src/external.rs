//! External programs: ffmpeg for HEIC, AVIF and video frames, gio for mounting and the programs
//! that open a file, xdg-open; inside a Flatpak they run on the host.

use crate::files::{AVIF_EXTS, HEIC_EXTS, has_ext};
use std::path::{Path, PathBuf};

/// HEIC and AVIF: both HEIF files, decoded the same way.
pub fn is_heif(path: &Path) -> bool {
    has_ext(path, HEIC_EXTS) || has_ext(path, AVIF_EXTS)
}

/// A frame `at` seconds in, decoded by the `ffmpeg` program.
#[cfg(any(feature = "video", feature = "heic", feature = "avif"))]
pub fn ffmpeg_frame(path: &Path, at: &str) -> Result<image::DynamicImage, String> {
    ffmpeg_ppm(path, &["-ss", at], &[])
}

/// A HEIC or AVIF photo. Phones store it as a grid of tiles, which ffmpeg reads as separate streams
/// with their places in a "Tile Grid" group: they are put together with xstack, cut to the
/// image size and turned by the stored rotation.
#[cfg(any(feature = "heic", feature = "avif"))]
pub fn heif(path: &Path) -> Result<image::DynamicImage, String> {
    let probe = command("ffprobe")
        .args(["-v", "error", "-show_stream_groups", "-of", "compact"])
        .args(FILES_ONLY)
        .arg(path)
        .output()
        .map_err(|e| format!("HEIC and AVIF need ffprobe (ffmpeg): {e}"))?;
    match heif_filter(&String::from_utf8_lossy(&probe.stdout)) {
        Some(filter) => ffmpeg_ppm(path, &[], &["-filter_complex", &filter, "-map", "[out]"]),
        None => ffmpeg_frame(path, "0"),
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

/// One frame of `path` as decoded by ffmpeg, with options before and after the input.
#[cfg(any(feature = "video", feature = "heic", feature = "avif"))]
pub fn ffmpeg_ppm(
    path: &Path,
    before: &[&str],
    after: &[&str],
) -> Result<image::DynamicImage, String> {
    let out = command("ffmpeg")
        .args(["-v", "error"])
        .args(before)
        .args(FILES_ONLY)
        .arg("-i")
        .arg(path)
        .args(after)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "ppm", "-"])
        .output()
        .map_err(|e| format!("video, HEIC and AVIF need ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(if err.is_empty() {
            "no frame".into()
        } else {
            err
        });
    }
    image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Pnm)
        .map_err(|e| e.to_string())
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
        Some(format!("Could not mount {uri}: {err}"))
    })
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
        .filter(|id| *id != "glimt.desktop");
    // Desktop files live in the "applications" folder of the XDG data folders.
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or("/usr/local/share:/usr/share".into());
    let dirs: Vec<PathBuf> = home
        .into_iter()
        .chain(dirs.split(':').map(PathBuf::from))
        .map(|d| d.join("applications"))
        .collect();
    ids.filter_map(|id| {
        let file = dirs.iter().map(|d| d.join(id)).find(|f| f.is_file())?;
        let text = std::fs::read_to_string(&file).ok()?;
        let name = text.lines().find_map(|l| l.strip_prefix("Name="))?;
        Some((name.to_owned(), file))
    })
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
    if std::env::var_os("FLATPAK_ID").is_none() {
        return std::process::Command::new(program);
    }
    let mut cmd = std::process::Command::new("flatpak-spawn");
    cmd.args(["--host", program]);
    cmd
}

#[cfg(test)]
mod tests {
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
