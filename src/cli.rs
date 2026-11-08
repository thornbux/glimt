//! The command line: the start options around the files and folders (`Start`), and the
//! commands that answer and end without opening a window (--help, --version, --check-config
//! and --back-up). The --remote commands are in remote.rs.

use crate::features::remote;
use crate::files::config::{self, Config, FADES};
use crate::viewer::view::LAYOUTS;
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
    (
        "--",
        "Every argument after it is a file or folder, also one starting with -",
    ),
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
    pub(crate) more_paths: Vec<PathBuf>,
    /// Whether `--` ended the options. Every argument after it is then a file or folder, also
    /// one named like an option or a command, such as `-h` or `--remote`.
    pub(crate) files_only: bool,
}

impl Start {
    /// Reads the start options in `args`, and returns them with the other arguments. The
    /// options come before the files and folders, and also after them, as Geeqie reads its
    /// own. After a file or folder only the start options are read: `--remote`, a command or
    /// an unknown `--word` there is a file or folder, as it always was. `--` ends the options.
    pub(crate) fn parse(mut args: Vec<OsString>) -> Result<(Start, Vec<OsString>), String> {
        let mut start = Start::default();
        while let Some(arg) = args.first().and_then(|a| a.to_str()).map(str::to_owned) {
            if let Some(taken) = start.take_option(&arg, args.get(1))? {
                args.drain(..taken);
                continue;
            }
            match arg.as_str() {
                "--" => {
                    start.files_only = true;
                    args.remove(0);
                    return Ok((start, args));
                }
                remote::REMOTE | remote::REMOTE_ALL => return Ok((start, args)),
                a if Command::named(a).is_some() => return Ok((start, args)),
                a if a.starts_with("--") => return Err(format!("unknown option {a}")),
                _ => break,
            }
        }
        // The first file or folder is here. Before, a start option after it was taken as one
        // more folder: `glimt a.jpg --fullscreen` showed the current folder with a.jpg's.
        let mut paths = Vec::with_capacity(args.len());
        let mut i = 0;
        while i < args.len() {
            let arg = args[i].to_str().unwrap_or_default().to_owned();
            if arg == "--" {
                start.files_only = true;
                paths.extend(args.drain(i + 1..));
                break;
            }
            match start.take_option(&arg, args.get(i + 1))? {
                Some(taken) => i += taken,
                None => {
                    paths.push(args[i].clone());
                    i += 1;
                }
            }
        }
        Ok((start, paths))
    }

    /// Takes the start option `arg`, with `value` after it for an option that has one.
    /// Returns how many arguments it took, or `None` when `arg` is no start option.
    fn take_option(
        &mut self,
        arg: &str,
        value: Option<&OsString>,
    ) -> Result<Option<usize>, String> {
        let value = value.and_then(|a| a.to_str()).unwrap_or_default();
        match arg {
            "--slideshow" => self.slideshow = true,
            "--random" => self.random = true,
            "--fullscreen" => self.fullscreen = true,
            "--kiosk" => self.kiosk = true,
            "--lock-settings" => self.lock_settings = true,
            "--fade" => {
                let k = FADES.iter().position(|f| f.0 == value);
                self.fade = Some(k.ok_or("--fade takes off, short or long")?);
                return Ok(Some(2));
            }
            "--layout" => {
                let layout = LAYOUTS.iter().find(|l| l.0 == value).ok_or_else(|| {
                    let all: Vec<&str> = LAYOUTS.iter().map(|l| l.0).collect();
                    format!("--layout takes one of {}", all.join(", "))
                })?;
                self.layout = Some(layout.0);
                return Ok(Some(2));
            }
            "--name" => {
                if !remote::valid_name(value) || value.is_empty() {
                    return Err(
                        "--name takes up to 64 letters, digits, - and _ (not only digits)".into(),
                    );
                }
                if !remote::name_fits(value) {
                    return Err(format!(
                        "--name {value} is too long for remote control here"
                    ));
                }
                self.name = Some(value.to_owned());
                return Ok(Some(2));
            }
            _ => return Ok(None),
        }
        Ok(Some(1))
    }
}

/// The commands that answer and end without opening a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Command {
    Version,
    CheckConfig,
    BackUp,
    Help,
}

impl Command {
    /// Returns the command that the argument `arg` names, or `None` for another argument.
    /// The parser stops at these names and `answer_without_window` acts on them, so both read
    /// the one list that the frozen options are checked against.
    pub(crate) fn named(arg: &str) -> Option<Command> {
        Some(match arg {
            "--version" | "-V" => Command::Version,
            "--check-config" => Command::CheckConfig,
            "--back-up" => Command::BackUp,
            "--help" | "-h" => Command::Help,
            _ => return None,
        })
    }
}

/// Answers the commands that need no window: --version, --check-config, --back-up and --help.
/// Returns true when one of them was given, and Glimt has nothing more to do. After `--` the
/// arguments are files, never a command.
pub(crate) fn answer_without_window(start: &Start, args_os: &[OsString]) -> bool {
    let first = (args_os.first().filter(|_| !start.files_only)).and_then(|a| a.to_str());
    match first.and_then(Command::named) {
        Some(Command::Version) => println!("glimt {}", env!("CARGO_PKG_VERSION")),
        Some(Command::CheckConfig) => check_config(args_os.get(1).map(PathBuf::from)),
        Some(Command::BackUp) => back_up(args_os.get(1)),
        Some(Command::Help) => print_help(),
        None => return false,
    }
    true
}

/// glimt --check-config [FILE]: what is wrong in the settings file, or FILE; exit 1 if anything.
fn check_config(file: Option<PathBuf>) -> ! {
    let Some(path) = file.or_else(config::config_path) else {
        eprintln!("glimt: no home folder, so no settings file");
        std::process::exit(1);
    };
    let problems = match crate::images::decode::read_text(&path) {
        Ok(text) => Config::parse_checked(&text).1,
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
    let actions: Vec<String> = (crate::ui::actions::SCRIPT_ACTIONS.iter())
        .map(|a| format!("{a:?}"))
        .collect();
    println!("\nActions for run:\n  {}", actions.join(" "));
    println!(
        "\nExit status of --remote: 0 done, {} no Glimt took it or none answered in time, {} \
         it cannot be sent, {} a window did not do it (the reason on standard error)",
        remote::NOT_TAKEN,
        remote::NOT_SENT,
        remote::NOT_DONE
    );
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

    /// After `--` every argument is a file or folder, also `-h` or one named like an option.
    /// Before, `glimt -- -h` was an unknown option, and a file named `--x.jpg` or `-h` could not
    /// be opened from the command line.
    #[test]
    fn files_follow_two_dashes() {
        let args = |a: &[&str]| {
            a.iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>()
        };
        let (s, rest) = Start::parse(args(&["--slideshow", "--", "-h", "--x.jpg"])).unwrap();
        assert!(s.slideshow && s.files_only);
        assert_eq!(rest, args(&["-h", "--x.jpg"]));
        assert!(
            !super::answer_without_window(&s, &rest),
            "-h after -- was taken as --help"
        );
        let (s, rest) = Start::parse(args(&["--", "--remote", "next"])).unwrap();
        assert!(s.files_only && !super::answer_without_window(&s, &rest));
        assert_eq!(rest, args(&["--remote", "next"]));
        assert!(!Start::parse(args(&["d"])).unwrap().0.files_only);
    }

    /// A start option after the file or folder is read as one, as Geeqie reads its options.
    /// Before, `glimt a.jpg --fullscreen` showed the current folder with a.jpg's, as if
    /// "--fullscreen" were a file there. Anything else after a file stays a file, as before, and
    /// `--` still ends the options.
    #[test]
    fn start_options_after_the_files_are_read() {
        let args = |a: &[&str]| {
            a.iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>()
        };
        let (s, rest) =
            Start::parse(args(&["a.jpg", "--fullscreen", "b", "--fade", "long"])).unwrap();
        assert!(s.fullscreen && s.fade == Some(2));
        assert_eq!(rest, args(&["a.jpg", "b"]));
        let (s, rest) = Start::parse(args(&["a.jpg", "--remote", "--x", "-h"])).unwrap();
        assert!(!s.fullscreen);
        assert_eq!(rest, args(&["a.jpg", "--remote", "--x", "-h"]));
        let (s, rest) = Start::parse(args(&["a.jpg", "--", "--random", "c"])).unwrap();
        assert!(s.files_only && !s.random);
        assert_eq!(rest, args(&["a.jpg", "--random", "c"]));
        assert!(Start::parse(args(&["a.jpg", "--layout", "9x9"])).is_err());
        // The arguments of --remote are its own.
        let (s, rest) = Start::parse(args(&["--remote", "open", "a", "--random"])).unwrap();
        assert!(!s.random);
        assert_eq!(rest, args(&["--remote", "open", "a", "--random"]));
    }
}
