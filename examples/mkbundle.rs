//! `cargo run --example mkbundle -- <home>` writes ONE Rust-created session bundle under
//! `<home>/.iota/sessions/` and prints its id on stdout — the session-format smoke sample.
//!
//! The bundle carries every record shape a real session produces (`iota::testing::every_record_shape`,
//! the same list `tests/session/roundtrip.rs` proves the store hands back): a system message, a user message
//! with an attachment, an assistant message with tool calls and a dialect raw payload, a tool result, a final
//! assistant with usage, and an interrupted partial. Point another reader at the bundle to see what the
//! on-disk format looks like today. Not a test, not shipped in the binary.

use std::{error::Error, path::PathBuf};

use iota::app::HostDirs;
use iota::provider::ProviderKind;
use iota::session::{NewSession, SessionStore};
use iota::testing::every_record_shape;

fn main() -> Result<(), Box<dyn Error>> {
    let home = std::env::args_os()
        .nth(1)
        .ok_or("usage: mkbundle <home directory>")?;
    let dirs = HostDirs {
        home: Some(PathBuf::from(home)),
        ..HostDirs::default()
    };
    let store = SessionStore::from_dirs(&dirs)?;
    let mut writer = store.create(NewSession {
        temperature: Some(0.7),
        base_url: "http://127.0.0.1:1/v1".to_owned(),
        cwd: "/tmp/mkbundle-cwd".to_owned(),
        ..NewSession::new(ProviderKind::OpenAi, "gpt-probe")
    })?;
    writer.update_meta(|meta| {
        "rust-created session".clone_into(&mut meta.title);
        "high".clone_into(&mut meta.effort);
        meta.context_window = 200_000;
    })?;
    writer.append_messages(&every_record_shape())?;
    println!("{}", writer.id());
    Ok(())
}
