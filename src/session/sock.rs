//! A running chat's socket: how another process puts a message into it (docs/design/agent-messaging.md, the
//! owner's decision of 2026-10-22 — a socket, not the inbox directory bot-mode.md had planned).
//!
//! **Where it lives.** One socket per interactive process, `<home>/.iota/run/<pid>.sock`, in a directory made
//! 0700 and checked to be ours; the socket itself is 0600. NOT inside the bundle, as the research sketched
//! (§4.2 B): a bundle sits under `sessions/projects/<slug of the project path>/`, and on this machine that is
//! already 110 bytes for a worktree — past the 104 a macOS socket address holds (`sun_path`, NUL included),
//! and a bundle directory is 0755, not private. The bundle gets a symlink instead, `<bundle>/sock` → the
//! socket ([`link`]): whoever knows the session (herdr is told the bundle path; `iota resume <id> -m` finds it
//! by id) finds the socket there, and connects to the link's TARGET ([`resolve`]) — the short path — so the
//! link's own length never matters. A chat that moves to another session (`/new`, `/session`, `/save`) moves
//! the link; the socket and `IOTA_SOCKET` stay what they are for the whole process.
//!
//! **Who may write.** The directory is private and the socket 0600, and every connection's peer is asked who
//! it is (`SO_PEERCRED` on Linux, `getpeereid`/`LOCAL_PEERCRED` on the BSDs and macOS, through tokio's
//! `peer_cred`, which needs no `unsafe`): a uid other than the socket's owner is refused before a byte is
//! read. Nothing listens on TCP. Windows has no listener at all in v1 (§2.4): a named pipe has no peer uid
//! to check, and the token that would stand in for it is a design of its own.
//!
//! **The wire.** One connection, one message, one receipt; UTF-8, one JSON object per line:
//!
//! ```text
//! → {"v":1,"type":"message","from":"ci","text":"build failed on main","reply":"/path/to/their.sock"}
//! ← {"ok":true,"state":"woken"}             // idle: it is the next turn's input
//! ← {"ok":true,"state":"queued","ahead":1}  // busy: behind 1 entry, a round boundary takes it
//! ← {"ok":false,"reason":"blocked"}         // the chat waits on the user (an approval): not taken
//! ← {"ok":false,"reason":"too-large"}       // the line is over MAX_LINE bytes
//! ← {"ok":false,"reason":"refused"}         // another uid, not this protocol, or an empty text
//! ← {"ok":false,"reason":"flooded"}         // over the rate, a repeat, or the queue is full
//! ```
//!
//! What the receiver does with a message — the notice it becomes, the limits, the hold — is the chat's
//! business (`repl::inbound`); this module is the mechanism both ends share.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::BoxFuture;

/// The bundle's link to the socket of the process that holds it.
pub const SOCK_LINK: &str = "sock";

/// The variable every child of an interactive chat carries: its socket's path (docs/design/agent-messaging.md
/// §2.3). A script the model starts can answer the chat it came from without knowing the session.
pub const IOTA_SOCKET: &str = "IOTA_SOCKET";

/// The protocol version a request carries (`"v"`).
pub const PROTOCOL: u64 = 1;

/// The longest request line, newline excluded; a longer one is answered `too-large`.
pub const MAX_LINE: usize = 64 * 1024;

/// How long either end waits on the other: the sender for its receipt, the receiver for the request line.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest receipt a sender reads.
#[cfg(unix)]
const MAX_RECEIPT: u64 = 4096;

/// One request: a message for the chat.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    /// [`PROTOCOL`].
    pub v: u64,
    /// `"message"` — the only type v1 has.
    #[serde(rename = "type")]
    pub kind: String,
    /// Who says it, in the sender's own words (the receiver shortens and cleans it).
    #[serde(default)]
    pub from: String,
    /// What it says.
    pub text: String,
    /// Where an answer may go, when the sender has an address (shown to the model, never used by iota).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
}

impl Request {
    /// A v1 message.
    pub fn message(from: &str, text: &str, reply: Option<String>) -> Self {
        Self {
            v: PROTOCOL,
            kind: "message".to_owned(),
            from: from.to_owned(),
            text: text.to_owned(),
            reply,
        }
    }

    /// The request's one line, without its newline.
    pub fn json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Why a message was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The chat waits on the user — an approval, an interactive tool, a held message.
    Blocked,
    /// The line is longer than [`MAX_LINE`].
    TooLarge,
    /// Another uid, a line that is not this protocol, or an empty text.
    Refused,
    /// Over the rate, a repeat of a message just taken, or the queue holds as many as it may.
    Flooded,
}

impl Reason {
    /// The receipt's `reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::TooLarge => "too-large",
            Self::Refused => "refused",
            Self::Flooded => "flooded",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [Self::Blocked, Self::TooLarge, Self::Refused, Self::Flooded]
            .into_iter()
            .find(|r| r.as_str() == s)
    }
}

/// What became of a message: the receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receipt {
    /// The idle chat took it as its next input.
    Woken,
    /// The busy chat queued it behind `ahead` entries.
    Queued {
        /// Entries before it.
        ahead: usize,
    },
    /// It was not taken.
    Refused(Reason),
}

impl Receipt {
    /// The receipt's one line, without its newline.
    pub fn json_line(&self) -> String {
        // Written out rather than through `json!`, whose map sorts the keys: the line reads in the order the
        // design doc prints it, `ok` first.
        match self {
            Self::Woken => r#"{"ok":true,"state":"woken"}"#.to_owned(),
            Self::Queued { ahead } => format!(r#"{{"ok":true,"state":"queued","ahead":{ahead}}}"#),
            Self::Refused(r) => format!(r#"{{"ok":false,"reason":"{}"}}"#, r.as_str()),
        }
    }

    /// A receipt line read back; `None` when it is not one.
    pub fn parse(line: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        if !v.get("ok")?.as_bool()? {
            return Reason::parse(v.get("reason")?.as_str()?).map(Self::Refused);
        }
        match v.get("state")?.as_str()? {
            "woken" => Some(Self::Woken),
            "queued" => Some(Self::Queued {
                ahead: usize::try_from(v.get("ahead").and_then(serde_json::Value::as_u64)?).ok()?,
            }),
            _ => None,
        }
    }

    /// Whether the message was taken.
    pub fn is_ok(&self) -> bool {
        !matches!(self, Self::Refused(_))
    }
}

/// The socket of process `pid` under `run_dir` (`HostDirs::run_dir`).
pub fn socket_path(run_dir: &Path, pid: u32) -> PathBuf {
    run_dir.join(format!("{pid}.sock"))
}

/// How many bytes a socket address holds, the terminating NUL included (`sun_path`).
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
const SUN_PATH: usize = 104;
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
const SUN_PATH: usize = 108;

/// Whether `path` can be a socket's address here: a Unix platform, and short enough for `sun_path`.
pub fn fits(path: &Path) -> bool {
    cfg!(unix) && path.as_os_str().len() < SUN_PATH
}

/// `<bundle>/sock`.
pub fn link_path(bundle: &Path) -> PathBuf {
    bundle.join(SOCK_LINK)
}

/// Who is on the other end of a connection, as the kernel says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Its effective uid.
    pub uid: u32,
    /// Its pid, where the platform says.
    pub pid: Option<i32>,
}

/// What the chat does with a message the socket admitted (`repl::inbound`): the receipt it answers.
pub trait Inbox: Send {
    /// Takes `req` from `peer` (already checked to be this uid, this protocol, a non-empty text).
    fn admit(&mut self, peer: Peer, req: Request) -> BoxFuture<'_, Receipt>;
}

/// The bundle's link now names `socket` (any earlier link is replaced).
#[cfg(unix)]
pub fn link(bundle: &Path, socket: &Path) -> std::io::Result<()> {
    clear(bundle)?;
    std::os::unix::fs::symlink(socket, link_path(bundle))
}

/// Removes the bundle's link, whatever it names — a link a crashed run left behind would otherwise send a
/// message to whichever process now has that pid. A file at that name that is not a link is left alone.
#[cfg(unix)]
pub fn clear(bundle: &Path) -> std::io::Result<()> {
    let path = link_path(bundle);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Removes the bundle's link when it still names `socket` — the exit, and a move to another session.
#[cfg(unix)]
pub fn unlink(bundle: &Path, socket: &Path) {
    let path = link_path(bundle);
    if std::fs::read_link(&path).is_ok_and(|t| t == socket) {
        let _ = std::fs::remove_file(path);
    }
}

/// The socket the bundle's link names, joined onto the bundle when relative.
#[cfg(unix)]
pub fn resolve(bundle: &Path) -> std::io::Result<PathBuf> {
    let target = std::fs::read_link(link_path(bundle))?;
    Ok(if target.is_relative() {
        bundle.join(target)
    } else {
        target
    })
}

/// No links where there is no socket.
#[cfg(not(unix))]
pub fn link(_bundle: &Path, _socket: &Path) -> std::io::Result<()> {
    Ok(())
}

/// No links where there is no socket.
#[cfg(not(unix))]
pub fn clear(_bundle: &Path) -> std::io::Result<()> {
    Ok(())
}

/// No links where there is no socket.
#[cfg(not(unix))]
pub fn unlink(_bundle: &Path, _socket: &Path) {}

/// No links where there is no socket.
#[cfg(not(unix))]
pub fn resolve(_bundle: &Path) -> std::io::Result<PathBuf> {
    Err(unsupported())
}

#[cfg(not(unix))]
fn unsupported() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "messaging a running session needs a Unix socket; this platform has none in iota yet",
    )
}

/// Sends `req` to the socket at `socket` and reads the receipt, all within [`IO_TIMEOUT`].
#[cfg(unix)]
pub async fn send(socket: &Path, req: &Request) -> std::io::Result<Receipt> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(socket).await?;
        let mut line = req.json_line();
        line.push('\n');
        stream.write_all(line.as_bytes()).await?;
        stream.shutdown().await?;
        let mut buf = Vec::new();
        (&mut stream)
            .take(MAX_RECEIPT)
            .read_to_end(&mut buf)
            .await?;
        let text = String::from_utf8_lossy(&buf);
        Receipt::parse(text.lines().next().unwrap_or_default()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("not a receipt: {:?}", text.trim_end()),
            )
        })
    };
    tokio::time::timeout(IO_TIMEOUT, exchange)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no receipt in time"))?
}

/// No socket to send to.
#[cfg(not(unix))]
#[allow(clippy::unused_async)] // the unix twin's signature: callers await either
pub async fn send(_socket: &Path, _req: &Request) -> std::io::Result<Receipt> {
    Err(unsupported())
}

/// A bound socket, removed when dropped.
#[cfg(unix)]
#[derive(Debug)]
pub struct Listener {
    inner: tokio::net::UnixListener,
    path: PathBuf,
    /// The uid a peer must have: the socket's owner, which is this process.
    uid: u32,
}

/// There is no socket on this platform (module doc).
#[cfg(not(unix))]
#[derive(Debug)]
pub enum Listener {}

#[cfg(unix)]
impl Listener {
    /// Binds `<run_dir>/<pid>.sock`: the directory made (or found) a 0700 directory that is ours, a stale
    /// socket of an earlier process with this pid removed, the new one made 0600. Must run inside the tokio
    /// runtime.
    pub fn bind(run_dir: &Path, pid: u32) -> std::io::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        match std::fs::symlink_metadata(run_dir) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(std::io::Error::other(format!(
                    "{} is not a directory",
                    run_dir.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(run_dir)?;
            }
            Err(e) => return Err(e),
        }
        std::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(0o700))?;
        let path = socket_path(run_dir, pid);
        // No live process shares this pid, so whatever sits at the name is a dead run's.
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let inner = tokio::net::UnixListener::bind(&path)?;
        let listener = Self {
            inner,
            uid: std::fs::metadata(&path)?.uid(),
            path,
        };
        std::fs::set_permissions(&listener.path, std::fs::Permissions::from_mode(0o600))?;
        if std::fs::metadata(run_dir)?.uid() != listener.uid {
            return Err(std::io::Error::other(format!(
                "{} belongs to another user",
                run_dir.display()
            )));
        }
        Ok(listener)
    }

    /// The socket's path (what `IOTA_SOCKET` and the bundle's link name).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The uid a peer must have.
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// A listener that admits `uid` alone — a peer of this process's own uid is then a stranger.
    #[cfg(test)]
    pub(crate) fn expecting_uid(mut self, uid: u32) -> Self {
        self.uid = uid;
        self
    }

    /// Serves connections one at a time until `cancel` fires (or the socket fails): each is answered with a
    /// receipt, then closed.
    pub async fn serve(&self, inbox: &mut dyn Inbox, cancel: &tokio_util::sync::CancellationToken) {
        loop {
            let stream = tokio::select! {
                () = cancel.cancelled() => return,
                r = self.inner.accept() => match r {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        tracing::debug!("iota socket: accept failed: {e}");
                        continue;
                    }
                },
            };
            if let Err(e) = self.answer(stream, inbox).await {
                tracing::debug!("iota socket: a connection failed: {e}");
            }
        }
    }

    /// One connection: who it is, the line, the receipt.
    async fn answer(
        &self,
        mut stream: tokio::net::UnixStream,
        inbox: &mut dyn Inbox,
    ) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        let receipt = match stream.peer_cred() {
            Ok(cred) if cred.uid() == self.uid => {
                let peer = Peer {
                    uid: cred.uid(),
                    pid: cred.pid(),
                };
                match tokio::time::timeout(IO_TIMEOUT, read_request(&mut stream)).await {
                    Ok(Ok(req)) => inbox.admit(peer, req).await,
                    Ok(Err(reason)) => Receipt::Refused(reason),
                    Err(_) => Receipt::Refused(Reason::Refused),
                }
            }
            // Another user, or a peer the kernel cannot name: nothing it sent is read.
            _ => Receipt::Refused(Reason::Refused),
        };
        let mut line = receipt.json_line();
        line.push('\n');
        stream.write_all(line.as_bytes()).await?;
        stream.shutdown().await?;
        // Whatever the sender still has in flight is read and dropped: a close with unread bytes resets the
        // connection on Linux, and the sender would lose the receipt it is about to read.
        let _ = tokio::time::timeout(IO_TIMEOUT, drain(&mut stream)).await;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(not(unix))]
impl Listener {
    /// Always unsupported (module doc).
    pub fn bind(_run_dir: &Path, _pid: u32) -> std::io::Result<Self> {
        Err(unsupported())
    }

    /// Unreachable: there is no listener.
    pub fn path(&self) -> &Path {
        match *self {}
    }

    /// Unreachable: there is no listener.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)] // the unix twin's signature: the loop awaits either
    pub async fn serve(
        &self,
        _inbox: &mut dyn Inbox,
        _cancel: &tokio_util::sync::CancellationToken,
    ) {
        match *self {}
    }
}

/// The request line, or why it is refused: longer than [`MAX_LINE`], not UTF-8, not JSON of this protocol,
/// or an empty text.
#[cfg(unix)]
async fn read_request(stream: &mut tokio::net::UnixStream) -> Result<Request, Reason> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut buf = Vec::new();
    let mut reader = tokio::io::BufReader::new(stream).take(MAX_LINE as u64 + 1);
    reader
        .read_until(b'\n', &mut buf)
        .await
        .map_err(|_| Reason::Refused)?;
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > MAX_LINE {
        return Err(Reason::TooLarge);
    }
    let req: Request = serde_json::from_slice(&buf).map_err(|_| Reason::Refused)?;
    if req.v != PROTOCOL || req.kind != "message" || req.text.trim().is_empty() {
        return Err(Reason::Refused);
    }
    Ok(req)
}

/// Reads to the end, dropping everything, up to a bound.
#[cfg(unix)]
async fn drain(stream: &mut tokio::net::UnixStream) {
    use tokio::io::AsyncReadExt;
    let mut sink = [0u8; 8192];
    let mut left: usize = 16 * 1024 * 1024;
    while left > 0 {
        match stream.read(&mut sink).await {
            Ok(0) | Err(_) => return,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        IO_TIMEOUT, Inbox, Listener, MAX_LINE, Peer, Reason, Receipt, Request, clear, link,
        link_path, resolve, send, socket_path, unlink,
    };
    use crate::BoxFuture;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    /// What a [`Taker`] took.
    type Taken = Arc<Mutex<Vec<(Peer, Request)>>>;

    /// Takes everything, answers `woken`, keeps what it took.
    #[derive(Default)]
    struct Taker(Taken);

    impl Inbox for Taker {
        fn admit(&mut self, peer: Peer, req: Request) -> BoxFuture<'_, Receipt> {
            self.0.lock().unwrap().push((peer, req));
            Box::pin(std::future::ready(Receipt::Woken))
        }
    }

    /// Serves `listener` with a [`Taker`] in the background; the cancel stops it.
    fn serve(listener: Listener) -> (Taken, CancellationToken) {
        let taken = Arc::default();
        let mut inbox = Taker(Arc::clone(&taken));
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        tokio::spawn(async move { listener.serve(&mut inbox, &stop).await });
        (taken, cancel)
    }

    /// One raw exchange: `bytes` out, the whole answer back.
    async fn raw(path: &std::path::Path, bytes: &[u8]) -> String {
        let mut s = tokio::net::UnixStream::connect(path)
            .await
            .expect("connect");
        s.write_all(bytes).await.expect("write");
        s.shutdown().await.expect("shutdown");
        let mut out = String::new();
        tokio::time::timeout(IO_TIMEOUT, s.read_to_string(&mut out))
            .await
            .expect("in time")
            .expect("read");
        out
    }

    /// Every receipt of §4.5 round-trips through its line, byte for byte as the design doc prints them.
    #[test]
    fn receipts_are_the_sketched_lines() {
        let cases = [
            (Receipt::Woken, r#"{"ok":true,"state":"woken"}"#),
            (
                Receipt::Queued { ahead: 1 },
                r#"{"ok":true,"state":"queued","ahead":1}"#,
            ),
            (
                Receipt::Refused(Reason::Blocked),
                r#"{"ok":false,"reason":"blocked"}"#,
            ),
            (
                Receipt::Refused(Reason::TooLarge),
                r#"{"ok":false,"reason":"too-large"}"#,
            ),
            (
                Receipt::Refused(Reason::Refused),
                r#"{"ok":false,"reason":"refused"}"#,
            ),
            (
                Receipt::Refused(Reason::Flooded),
                r#"{"ok":false,"reason":"flooded"}"#,
            ),
        ];
        for (receipt, line) in cases {
            assert_eq!(receipt.json_line(), line);
            assert_eq!(Receipt::parse(line), Some(receipt));
        }
        assert_eq!(Receipt::parse(r#"{"ok":true,"state":"maybe"}"#), None);
        assert_eq!(Receipt::parse("nope"), None);
        assert_eq!(
            Request::message("ci", "hi", None).json_line(),
            r#"{"v":1,"type":"message","from":"ci","text":"hi"}"#
        );
    }

    /// The directory is 0700 and the socket 0600; a stale socket of the same name is replaced; the drop
    /// removes it.
    #[tokio::test]
    async fn the_socket_is_private_and_goes_with_the_listener() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        std::fs::create_dir(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = socket_path(&run, 4242);
        std::fs::write(&path, "stale").unwrap();
        let l = Listener::bind(&run, 4242).expect("bind");
        assert_eq!(l.path(), path);
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&run), 0o700, "the directory is private");
        assert_eq!(mode(&path), 0o600, "the socket is the owner's alone");
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_socket()
        );
        drop(l);
        assert!(!path.exists(), "the socket is removed with its listener");
    }

    /// A directory at the run path that is a symlink is not followed.
    #[tokio::test]
    async fn a_symlinked_run_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let run = tmp.path().join("run");
        std::os::unix::fs::symlink(&elsewhere, &run).unwrap();
        Listener::bind(&run, 1).expect_err("a link is not our directory");
    }

    /// A message travels and is answered; the inbox sees the peer as this process's uid.
    #[tokio::test]
    async fn a_message_is_admitted_and_answered() {
        let tmp = tempfile::tempdir().unwrap();
        let l = Listener::bind(tmp.path(), 7).unwrap();
        let (path, uid) = (l.path().to_owned(), l.uid());
        let (taken, cancel) = serve(l);
        let receipt = send(&path, &Request::message("ci", "build failed", None))
            .await
            .expect("receipt");
        assert_eq!(receipt, Receipt::Woken);
        let taken = taken.lock().unwrap().clone();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0.uid, uid);
        assert_eq!(taken[0].1.text, "build failed");
        cancel.cancel();
    }

    /// RED/GREEN for the peer check: the same exchange against a listener that expects another uid is
    /// refused, and the inbox never sees it.
    #[tokio::test]
    async fn another_uid_is_refused_unread() {
        let tmp = tempfile::tempdir().unwrap();
        let l = Listener::bind(tmp.path(), 8).unwrap();
        let stranger = l.uid().wrapping_add(1);
        let l = l.expecting_uid(stranger);
        let path = l.path().to_owned();
        let (taken, cancel) = serve(l);
        let receipt = send(&path, &Request::message("ci", "rm -rf ~", None))
            .await
            .expect("receipt");
        assert_eq!(receipt, Receipt::Refused(Reason::Refused));
        assert!(taken.lock().unwrap().is_empty(), "nothing was admitted");
        cancel.cancel();
    }

    /// A line over the cap is `too-large` (and the sender still reads its receipt); one at the cap is read;
    /// what is not this protocol is `refused`.
    #[tokio::test]
    async fn the_line_is_bounded_and_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let l = Listener::bind(tmp.path(), 9).unwrap();
        let path = l.path().to_owned();
        let (taken, cancel) = serve(l);

        let big = "x".repeat(MAX_LINE);
        let receipt = send(&path, &Request::message("ci", &big, None))
            .await
            .expect("a receipt even for a huge line");
        assert_eq!(receipt, Receipt::Refused(Reason::TooLarge));

        let fits = Request::message("ci", &"y".repeat(MAX_LINE - 100), None);
        assert!(fits.json_line().len() <= MAX_LINE);
        assert_eq!(send(&path, &fits).await.unwrap(), Receipt::Woken);

        for bad in [
            &b"not json\n"[..],
            br#"{"v":2,"type":"message","text":"hi"}"#,
            br#"{"v":1,"type":"steer","text":"hi"}"#,
            br#"{"v":1,"type":"message","text":"   "}"#,
            b"\xff\xfe\n",
        ] {
            assert_eq!(
                raw(&path, bad).await,
                "{\"ok\":false,\"reason\":\"refused\"}\n",
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(taken.lock().unwrap().len(), 1, "only the line that fits");
        cancel.cancel();
    }

    /// The bundle's link names the socket, is replaced by a newer one, resolves to the short target, and is
    /// removed only by the process it names; a file that is not a link is never touched.
    #[test]
    fn the_bundle_link() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("b");
        std::fs::create_dir(&bundle).unwrap();
        let (a, b) = (tmp.path().join("1.sock"), tmp.path().join("2.sock"));
        link(&bundle, &a).unwrap();
        assert_eq!(resolve(&bundle).unwrap(), a);
        link(&bundle, &b).unwrap();
        assert_eq!(resolve(&bundle).unwrap(), b);
        unlink(&bundle, &a);
        assert_eq!(resolve(&bundle).unwrap(), b, "not ours to remove");
        unlink(&bundle, &b);
        assert!(resolve(&bundle).is_err());
        std::fs::write(link_path(&bundle), "mine").unwrap();
        clear(&bundle).unwrap();
        assert!(link_path(&bundle).exists());
    }
}
