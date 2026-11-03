//! The command line: the start options in front of the file or folder (`Start`), and the
//! commands that answer and end without opening a window (--help, --version, --check-config
//! and --back-up). The --remote commands are in remote.rs.

use crate::files::config::{self, Config, FADES};
use crate::remote;
use crate::view::LAYOUTS;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Options on the command line before the file or folder, for --help.
pub(crate) const START_OPTIONS: &[(&str, &str)] = &[
    (
        "--slideshow",
        "Start the slideshow; of a list (.gqv, .txt, .m3u), in its order",
    ),
    (
        "--random",
        "Random order in the slideshow (kept, as in the View menu)",
    ),
    ("--fullscreen", "Start in full screen"),
    (
        "--kiosk",
        "Full screen, only the image, nothing changes files; ends by --remote quit",
    ),
    (
        "--lock-settings",
        "No Preferences, and changes to settings last only until Glimt closes",
    ),
    ("--layout 1|2x1|1x2|2x2", "Panes as in View → Layout (kept)"),
    (
        "--fade off|short|long",
        "Fade between slideshow images (kept)",
    ),
    (
        "--name NAME",
        "Name this Glimt; glimt --name NAME --remote COMMAND reaches only it",
    ),
    (
        "--check-config [FILE]",
        "Tell what is wrong in the settings file (or FILE), line by line",
    ),
    (
        "--back-up FOLDER",
        "Copy Glimt's data to a new folder glimt-backup-DATE in FOLDER",
    ),
    ("-V, --version", "Print the version"),
    ("-h, --help", "Print this help"),
];

/// What the start options ask for.
#[derive(Clone, Default)]
pub(crate) struct Start {
    pub(crate) slideshow: bool,
    pub(crate) random: bool,
    pub(crate) fullscreen: bool,
    pub(crate) layout: Option<&'static str>,
    pub(crate) name: Option<String>,
    pub(crate) kiosk: bool,
    pub(crate) lock_settings: bool,
    /// --fade: an index in FADES.
    pub(crate) fade: Option<usize>,
    /// More folders after the first, to show together.
    pub(crate) more: Vec<PathBuf>,
}

impl Start {
    /// The options in front of `args`, and the arguments after them.
    pub(crate) fn parse(mut args: Vec<OsString>) -> Result<(Start, Vec<OsString>), String> {
        let mut start = Start::default();
        while let Some(arg) = args.first().and_then(|a| a.to_str()).map(str::to_owned) {
            match arg.as_str() {
                "--slideshow" => start.slideshow = true,
                "--random" => start.random = true,
                "--fullscreen" => start.fullscreen = true,
                "--kiosk" => start.kiosk = true,
                "--lock-settings" => start.lock_settings = true,
                "--fade" => {
                    let value = args.get(1).and_then(|a| a.to_str()).unwrap_or_default();
                    let k = FADES.iter().position(|f| f.0 == value);
                    start.fade = Some(k.ok_or("--fade takes off, short or long")?);
                    args.remove(0);
                }
                "--layout" => {
                    let value = args.get(1).and_then(|a| a.to_str()).unwrap_or_default();
                    let layout = LAYOUTS.iter().find(|l| l.0 == value).ok_or_else(|| {
                        let all: Vec<&str> = LAYOUTS.iter().map(|l| l.0).collect();
                        format!("--layout takes one of {}", all.join(", "))
                    })?;
                    start.layout = Some(layout.0);
                    args.remove(0);
                }
                "--name" => {
                    let name = args.get(1).and_then(|a| a.to_str()).unwrap_or_default();
                    if !remote::valid_name(name) || name.is_empty() {
                        return Err(
                            "--name takes up to 64 letters, digits, - and _ (not only digits)"
                                .into(),
                        );
                    }
                    if !remote::name_fits(name) {
                        return Err(format!("--name {name} is too long for remote control here"));
                    }
                    start.name = Some(name.to_owned());
                    args.remove(0);
                }
                "--help" | "-h" | "--version" | "-V" | "--remote" | "--remote-all"
                | "--check-config" | "--back-up" => break,
                a if a.starts_with("--") => return Err(format!("unknown option {a}")),
                _ => break,
            }
            args.remove(0);
        }
        Ok((start, args))
    }
}

/// Answers the commands that need no window: --version, --check-config, --back-up and --help.
/// Returns true when one of them was given, and Glimt has nothing more to do.
pub(crate) fn answered(args_os: &[OsString]) -> bool {
    let first = args_os.first().and_then(|a| a.to_str());
    match first {
        Some("--version" | "-V") => println!("glimt {}", env!("CARGO_PKG_VERSION")),
        Some("--check-config") => check_config(args_os.get(1).map(PathBuf::from)),
        Some("--back-up") => back_up(args_os.get(1)),
        Some("--help" | "-h") => print_help(),
        _ => return false,
    }
    true
}

/// glimt --check-config [FILE]: what is wrong in the settings file, or FILE; exit 1 if anything.
fn check_config(file: Option<PathBuf>) -> ! {
    let Some(path) = file.or_else(config::config_path) else {
        eprintln!("glimt: no home folder, so no settings file");
        std::process::exit(1);
    };
    let problems = match crate::decode::read_text(&path) {
        Ok(text) => Config::check(&text).1,
        Err(e) => vec![format!("cannot be read ({e})")],
    };
    for p in &problems {
        eprintln!("{}: {p}", path.display());
    }
    if problems.is_empty() {
        println!("{}: OK", path.display());
    }
    std::process::exit(!problems.is_empty() as i32)
}

/// Copies Glimt's data into a new folder in `dir` for glimt --back-up DIR. It ends Glimt with
/// exit code 2 when the folder is missing, and with 1 when the backup fails.
fn back_up(dir: Option<&OsString>) {
    let Some(dir) = dir else {
        eprintln!("glimt: --back-up takes the folder to put the backup in");
        std::process::exit(2);
    };
    match crate::files::backup::back_up_to(Path::new(dir)) {
        Ok(done) => println!("{done}"),
        Err(e) => {
            eprintln!("glimt: {e}");
            std::process::exit(1);
        }
    }
}

/// Prints the usage, the start options and the --remote commands for glimt --help.
fn print_help() {
    println!(
        "Usage: glimt [OPTIONS] [FILE_OR_FOLDER …]   several folders show as one list\n       glimt --remote COMMAND      to the Glimt used last\n       glimt --remote-all COMMAND  to every running Glimt\n\nOptions:"
    );
    for (option, does) in START_OPTIONS {
        println!("  {option:<24} {does}");
    }
    println!("\nCommands:");
    for (command, does) in remote::COMMANDS {
        println!("  {command:<12} {does}");
    }
}

#[cfg(test)]
mod tests {
    use super::Start;

    /// Start options in front of the file or folder; the rest is left as it was.
    #[test]
    fn parses_start_options() {
        let args = |a: &[&str]| {
            a.iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>()
        };
        let (s, rest) = Start::parse(args(&[
            "--fullscreen",
            "--layout",
            "2x2",
            "--slideshow",
            "d",
        ]))
        .unwrap();
        assert!(s.fullscreen && s.slideshow && !s.random && s.layout == Some("2x2"));
        assert_eq!(rest, args(&["d"]));
        let (s, rest) = Start::parse(args(&["--random", "--remote", "next"])).unwrap();
        assert!(s.random && s.layout.is_none());
        assert_eq!(rest, args(&["--remote", "next"]));
        assert!(Start::parse(args(&["--layout", "3x3"])).is_err());
        assert!(Start::parse(args(&["--layout"])).is_err());
        assert!(Start::parse(args(&["--bogus"])).is_err());
        let (s, rest) = Start::parse(args(&["--name", "left", "--remote", "next"])).unwrap();
        assert_eq!(
            (s.name.as_deref(), rest),
            (Some("left"), args(&["--remote", "next"]))
        );
        assert!(Start::parse(args(&["--name", "123"])).is_err());
        assert!(Start::parse(args(&["--name", "a/b"])).is_err());
        assert!(Start::parse(args(&["--name"])).is_err());
        assert!(Start::parse(args(&["--kiosk", "d"])).unwrap().0.kiosk);
        assert!(
            Start::parse(args(&["--lock-settings"]))
                .unwrap()
                .0
                .lock_settings
        );
        assert_eq!(
            Start::parse(args(&["--fade", "long"])).unwrap().0.fade,
            Some(2)
        );
        assert!(Start::parse(args(&["--fade", "slow"])).is_err());
        let (_, rest) = Start::parse(args(&["--version"])).unwrap();
        assert_eq!(rest, args(&["--version"]));
    }
}
