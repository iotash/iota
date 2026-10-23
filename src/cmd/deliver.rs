//! `-m` against a session another process holds (docs/design/agent-messaging.md §5): no new verb. `iota resume
//! <id> -m` on a bundle whose lock is held, and `iota run <bot> -m` on a bot that is running, hand the message to
//! the holder through its socket (`session::sock`) instead of refusing with `SessionError::Locked` /
//! `ArgsError::BotHeadless` — and print the holder's receipt on stdout, one JSON line, exactly as the holder
//! wrote it. Only its `ok` is read: a receipt that is not `ok` is the run's error too (exit 1), so a script can
//! test the status alone.
//!
//! The message says who sent it — `iota -m` — and, when the sender is itself a child of a chat (it carries that
//! chat's `IOTA_SOCKET`), where an answer may go: one chat's model telling another's something gets a reply
//! address for free.

use std::io::Write as _;
use std::path::Path;

use crate::app::env::Env;
use crate::cmd::{ArgsError, CliError, RunError, io};
use crate::session::sock::{self, Request};

/// The `from` a message sent by `-m` carries.
pub(crate) const FROM: &str = "iota -m";

/// Hands `text` to the process that holds the bundle at `bundle` (`what` names it, `pid` is what its lock
/// says) and prints the receipt.
pub(crate) async fn to_holder(
    bundle: &Path,
    what: String,
    pid: Option<u32>,
    text: &str,
    env: &Env,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    let unreachable = |source| RunError::Unreachable {
        what: what.clone(),
        pid,
        source,
    };
    let socket = sock::resolve(bundle).map_err(unreachable)?;
    let reply = env
        .var(sock::IOTA_SOCKET)
        .filter(|s| !s.is_empty() && Path::new(s) != socket);
    let answer = sock::send(&socket, &Request::message(FROM, text, reply))
        .await
        .map_err(unreachable)?;
    writeln!(io.stdout, "{}", answer.line)?;
    if answer.ok {
        Ok(())
    } else {
        Err(RunError::NotDelivered {
            what,
            receipt: answer.line,
        }
        .into())
    }
}

/// `iota run <bot> -m`: a running bot is handed the message; one that is not running keeps the refusal it
/// always had (a bot is interactive-only, bot-mode.md §2.2).
pub(crate) async fn to_bot(
    name: String,
    message: Option<&str>,
    env: &Env,
    stdin: &mut dyn std::io::Read,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    let refusal = || ArgsError::BotHeadless { name: name.clone() };
    let Some(bot_dir) = env.dirs.bots_dir().map(|d| d.join(&name)) else {
        return Err(refusal().into());
    };
    let Some(pid) = crate::session::SessionStore::bot_holder(&bot_dir, &name)? else {
        return Err(refusal().into());
    };
    let text = match message {
        Some("-") => crate::cmd::resolve::read_stdin_message(stdin)?,
        Some("") | None => return Err(ArgsError::MessageEmpty.into()),
        Some(m) => m.to_owned(),
    };
    let what = format!("bot {name}");
    let Some(pointer) = crate::session::BotPointer::read(&bot_dir)? else {
        return Err(RunError::Unreachable {
            what,
            pid,
            source: std::io::Error::other("it has no session yet"),
        }
        .into());
    };
    let store = crate::session::SessionStore::from_dirs(&env.dirs)?;
    let bundle = store.dir(&pointer.session)?;
    to_holder(&bundle, what, pid, &text, env, io).await
}
