//! The desktop portals that need what gdbus cannot do: a file descriptor passed, or the answer
//! of a dialog awaited. Used in the Flatpak, where Glimt reaches the printers through them.

use std::collections::HashMap;
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";

const PATH: &str = "/org/freedesktop/portal/desktop";

/// How long a request waits for the portal's answer. The user may sit in a dialog for long,
/// but a portal backend that died after the call never answers, and the work waiting on it holds
/// the busy pointer and keeps the window from closing.
const ANSWER_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

type Results = HashMap<String, OwnedValue>;

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
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let e = |e: zbus::Error| e.to_string();
    let sender = conn.unique_name().ok_or("no D-Bus name")?;
    let sender = sender.trim_start_matches(':').replace('.', "_");
    let handle = format!("{PATH}/request/{sender}/{token}");
    // The portal's changes of owner are listened for before the Ping, so that a restart right
    // after it is not missed.
    let owners = MessageIterator::for_match_rule(owner_rule().map_err(e)?, conn, Some(4));
    let mut owners = owners.map_err(e)?.into_inner();
    // The portal's unique name is the sender of its answer to a Ping, which also starts it.
    let peer = Some("org.freedesktop.DBus.Peer");
    let pong = conn
        .call_method(Some(DEST), PATH, peer, "Ping", &())
        .map_err(e)?;
    let owner = pong.header().sender().ok_or("no D-Bus name")?.to_string();
    let rule = response_rule(&owner, &handle).map_err(e)?;
    let responses = MessageIterator::for_match_rule(rule, conn, Some(1)).map_err(e)?;
    let mut responses = responses.into_inner();
    conn.call_method(Some(DEST), PATH, Some(interface), method, body)
        .map_err(e)?;
    let stop = || {
        // The dialog, if one is still open, goes too, so it cannot act after the time is up.
        let request = Some("org.freedesktop.portal.Request");
        let _ = conn.call_method(Some(DEST), handle.as_str(), request, "Close", &());
        // A closed request sends no Response. Closing the connection ends the wait for one.
        let _ = conn.clone().close();
    };
    // The Response, or an end to the wait when the portal that was asked has left the bus. A
    // portal that restarted has lost the request, and its new name is never trusted to answer.
    let wait = move || {
        use futures_lite::Stream;
        use std::task::Poll;
        // A blocking iterator waits on one stream only. block_on waits here on both streams at
        // once, without a thread for each.
        futures_lite::future::block_on(std::future::poll_fn(|cx| {
            while let Poll::Ready(change) = std::pin::Pin::new(&mut owners).poll_next(cx) {
                match change {
                    Some(Ok(change)) if !portal_lost(&change, &owner) => continue,
                    _ => return Poll::Ready(Err(())),
                }
            }
            std::pin::Pin::new(&mut responses).poll_next(cx).map(Ok)
        }))
    };
    let Some(message) = within(ANSWER_LIMIT, wait, stop) else {
        return Err("no answer from the portal".into());
    };
    let message = message.map_err(|()| "the portal restarted and lost the request")?;
    let message = message.ok_or("no answer from the portal")?.map_err(e)?;
    let (code, results): (u32, Results) = message.body().deserialize().map_err(e)?;
    // 0 done, 1 cancelled, 2 failed.
    match code {
        0 => Ok(Some(results)),
        1 => Ok(None),
        _ => Err(format!("{interface}.{method} failed")),
    }
}

/// Returns the match rule for the Response to the request at `path`, sent by `owner`, the
/// portal's unique name on the bus. The path is easy to guess, so another program on the bus
/// could send a Response first. zbus checks the sender of a signal only by a unique name.
fn response_rule<'a>(owner: &'a str, path: &'a str) -> zbus::Result<zbus::MatchRule<'a>> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(owner)?
        .interface("org.freedesktop.portal.Request")?
        .member("Response")?
        .path(path)?;
    Ok(rule.build())
}

/// Returns the match rule for the bus's NameOwnerChanged signals about the portal's name.
fn owner_rule() -> zbus::Result<zbus::MatchRule<'static>> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, DEST)?;
    Ok(rule.build())
}

/// Returns whether `change`, a NameOwnerChanged signal, says that the portal's name no longer
/// belongs to `owner`, the portal that was asked. The portal's own start by the Ping gives the
/// name to `owner`, so it does not count. A signal that another program sent straight to Glimt
/// does not count either, as the bus itself is the sender of the real ones.
fn portal_lost(change: &zbus::message::Message, owner: &str) -> bool {
    let header = change.header();
    let from_bus = header
        .sender()
        .is_some_and(|s| s.as_str() == "org.freedesktop.DBus");
    let body = change.body();
    let Ok((name, _, new)) = body.deserialize::<(&str, &str, &str)>() else {
        return false;
    };
    from_bus && name == DEST && new != owner
}

/// Runs `wait` in a thread of its own. Returns what it gives, or `None` when it takes longer
/// than `limit`. Then `stop` is called to end the wait, and the thread is given a second to
/// end, so that it is not left blocked for good.
fn within<T: Send + 'static>(
    limit: std::time::Duration,
    wait: impl FnOnce() -> T + Send + 'static,
    stop: impl FnOnce(),
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(wait()));
    let answer = rx.recv_timeout(limit).ok();
    if answer.is_none() {
        stop();
        let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
    }
    answer
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
        // A backend that does not name it: the dialog, for the message.
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| crate::support::lang::tr(crate::features::print::SYSTEM_DIALOG).into());
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
/// Waits for the choice: false when the user cancelled.
pub fn open_with(path: &std::path::Path) -> Result<bool, String> {
    let conn = Connection::session().map_err(|e| e.to_string())?;
    // Only for its descriptor, which says which file: the portal opens it itself. It is
    // opened as every file is, so a pipe named in a collection is refused rather than waited on.
    let file = crate::images::decode::open(path).map_err(|e| e.to_string())?;
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
    .map(|r| r.is_some())
}

#[cfg(test)]
mod tests {
    /// A Response counts only when the portal itself sent it, not another program on the bus.
    #[test]
    fn only_the_portal_answers() {
        let path = "/org/freedesktop/portal/desktop/request/1_2/glimt1_0";
        let rule = super::response_rule(":1.5", path).unwrap();
        let from = |sender: &str| {
            let signal =
                zbus::message::Message::signal(path, "org.freedesktop.portal.Request", "Response");
            signal.unwrap().sender(sender).unwrap().build(&()).unwrap()
        };
        assert!(rule.matches(&from(":1.5")).unwrap());
        assert!(!rule.matches(&from(":1.6")).unwrap());
    }

    /// A portal that restarted or left the bus ends the wait for its answer, while its own start
    /// and a signal that another program forged do not.
    #[test]
    fn a_portal_that_restarts_ends_the_wait() {
        let change = |sender: &str, name: &str, new: &str| {
            let signal = zbus::message::Message::signal(
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameOwnerChanged",
            );
            let signal = signal.unwrap().sender(sender).unwrap();
            signal.build(&(name, ":1.4", new)).unwrap()
        };
        let bus = "org.freedesktop.DBus";
        let portal = super::DEST;
        assert!(super::portal_lost(&change(bus, portal, ":1.9"), ":1.5"));
        assert!(super::portal_lost(&change(bus, portal, ""), ":1.5"));
        assert!(!super::portal_lost(&change(bus, portal, ":1.5"), ":1.5"));
        assert!(!super::portal_lost(
            &change(bus, "org.example.Other", ""),
            ":1.5"
        ));
        assert!(!super::portal_lost(&change(":1.6", portal, ""), ":1.5"));
        let rule = super::owner_rule().unwrap();
        assert!(rule.matches(&change(bus, portal, "")).unwrap());
        assert!(!rule.matches(&change(bus, "org.example.Other", "")).unwrap());
    }

    /// A wait for a portal's answer ends at its limit, also when the answer never comes, and
    /// the thread that waited ends too.
    #[test]
    fn a_wait_for_an_answer_ends() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (keep, never) = std::sync::mpsc::channel::<()>();
        let ended = std::sync::Arc::new(AtomicBool::new(false));
        let waiting = ended.clone();
        let wait = move || {
            let answer = never.recv().ok();
            waiting.store(true, Ordering::SeqCst);
            answer
        };
        let limit = std::time::Duration::from_millis(100);
        // The stop sends the answer, while the channel stays open for the whole test.
        let stop = || keep.send(()).unwrap();
        assert_eq!(super::within(limit, wait, stop), None);
        assert!(
            ended.load(Ordering::SeqCst),
            "the waiting thread is left blocked"
        );
        assert_eq!(super::within(limit, || 7, || ()), Some(7));
    }
}
