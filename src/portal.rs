//! The desktop portals that need what gdbus cannot do: a file descriptor passed, or the answer
//! of a dialog awaited. Used in the Flatpak, where Glimt reaches the printers through them.

use std::collections::HashMap;
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

type Results = HashMap<String, OwnedValue>;

/// Whether Glimt runs in a Flatpak, where these portals stand in for the host's programs.
pub fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// `method` of the portal `interface` called with `body`, whose options carry `token` as their
/// handle_token, and its request's Response awaited: the results, or `None` when the user
/// cancelled. The Response is listened for before the call, so it cannot come first.
fn request<B>(
    conn: &Connection,
    interface: &str,
    method: &str,
    token: &str,
    body: &B,
) -> Result<Option<Results>, String>
where
    B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
{
    let e = |e: zbus::Error| e.to_string();
    let sender = conn.unique_name().ok_or("no D-Bus name")?;
    let sender = sender.trim_start_matches(':').replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{token}");
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.portal.Request")
        .and_then(|r| r.member("Response"))
        .and_then(|r| r.path(path))
        .map_err(e)?
        .build();
    let mut responses = MessageIterator::for_match_rule(rule, conn, Some(1)).map_err(e)?;
    conn.call_method(Some(DEST), PATH, Some(interface), method, body)
        .map_err(e)?;
    let message = responses
        .next()
        .ok_or("no answer from the portal")?
        .map_err(e)?;
    let (code, results): (u32, Results) = message.body().deserialize().map_err(e)?;
    // 0 done, 1 cancelled, 2 failed.
    match code {
        0 => Ok(Some(results)),
        1 => Ok(None),
        _ => Err(format!("{interface}.{method} failed")),
    }
}

/// A token for a request, new for each.
fn token() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "glimt{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// The PDF that `write` makes, printed through the print portal: its dialog first, with
/// `copies` and `sides` (CUPS's two-sided-long-edge or -short-edge) filled in. The printer
/// chosen, or `None` when the dialog was cancelled.
pub fn print(
    copies: u32,
    sides: Option<&str>,
    title: &str,
    write: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
) -> Result<Option<String>, String> {
    let conn = Connection::session().map_err(|e| e.to_string())?;
    const PRINT: &str = "org.freedesktop.portal.Print";
    // GtkPrintSettings, whose values are all strings.
    let duplex = match sides {
        Some("two-sided-short-edge") => "vertical",
        Some(_) => "horizontal",
        None => "simplex",
    };
    let copies = copies.to_string();
    let settings = HashMap::from([
        ("n-copies", Value::from(copies.as_str())),
        ("duplex", Value::from(duplex)),
    ]);
    let ask = token();
    let options = HashMap::from([("handle_token", Value::from(ask.as_str()))]);
    let none: HashMap<&str, Value> = HashMap::new();
    let body = ("", title, settings, none, options);
    let Some(chosen) = request(&conn, PRINT, "PreparePrint", &ask, &body)? else {
        return Ok(None);
    };
    let printer = (chosen.get("settings"))
        .and_then(|s| HashMap::<String, OwnedValue>::try_from(s.try_clone().ok()?).ok())
        .and_then(|s| String::try_from(s.get("printer")?.try_clone().ok()?).ok())
        .unwrap_or_default();
    let ticket = (chosen.get("token"))
        .and_then(|t| u32::try_from(t).ok())
        .ok_or("no token from the print dialog")?;
    // The PDF in a file of no name, so nothing is left behind: removed once open, and read by
    // the portal through its descriptor.
    let at = std::env::temp_dir().join(format!("{}.pdf", token()));
    #[allow(clippy::disallowed_methods)] // a file of no name, which only the portal reads
    let mut file = std::fs::File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&at)
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&at);
    {
        let mut out = std::io::BufWriter::new(&mut file);
        write(&mut out).map_err(|e| e.to_string())?;
        std::io::Write::flush(&mut out).map_err(|e| e.to_string())?;
    }
    use std::io::Seek;
    file.rewind().map_err(|e| e.to_string())?;
    let send = token();
    let options = HashMap::from([
        ("handle_token", Value::from(send.as_str())),
        ("token", Value::from(ticket)),
    ]);
    use std::os::fd::AsFd;
    let fd = zbus::zvariant::Fd::from(file.as_fd());
    let body = ("", title, fd, options);
    match request(&conn, PRINT, "Print", &send, &body)? {
        Some(_) => Ok(Some(printer)),
        None => Ok(None),
    }
}

/// The screen kept from going idle (no screen saver, no blanking) through the inhibit portal,
/// until the sender returned is dropped: the request is closed then.
pub fn inhibit_idle(reason: &str) -> Result<std::sync::mpsc::Sender<()>, String> {
    let e = |e: zbus::Error| e.to_string();
    let conn = Connection::session().map_err(e)?;
    let token = token();
    let options = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("reason", Value::from(reason)),
    ]);
    // 8: idle.
    let body = ("", 8u32, options);
    let reply = (conn.call_method(
        Some(DEST),
        PATH,
        Some("org.freedesktop.portal.Inhibit"),
        "Inhibit",
        &body,
    ))
    .map_err(e)?;
    let handle: zbus::zvariant::OwnedObjectPath = reply.body().deserialize().map_err(e)?;
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        // Until the sender is dropped; the connection, and with it the inhibition, lives here.
        let _ = rx.recv();
        let request = Some("org.freedesktop.portal.Request");
        let _ = conn.call_method(Some(DEST), handle.as_str(), request, "Close", &());
    });
    Ok(tx)
}

/// `path` opened in an application the user picks in the desktop's chooser (the open URI
/// portal, asked to ask): Open With in the Flatpak, which cannot list the host's applications.
/// Waits for the choice.
pub fn open_with(path: &std::path::Path) -> Result<(), String> {
    let conn = Connection::session().map_err(|e| e.to_string())?;
    // Only for its descriptor, which says which file: the portal opens it itself.
    #[allow(clippy::disallowed_methods)]
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let token = token();
    let options = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("ask", Value::from(true)),
    ]);
    use std::os::fd::AsFd;
    let body = ("", zbus::zvariant::Fd::from(file.as_fd()), options);
    request(
        &conn,
        "org.freedesktop.portal.OpenURI",
        "OpenFile",
        &token,
        &body,
    )
    .map(drop)
}
