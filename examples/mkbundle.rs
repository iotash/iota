//! `cargo run --example mkbundle -- <home>` writes ONE Rust-created session bundle under
//! `<home>/.iota/sessions/` and prints its id on stdout.
//!
//! It is the input to the cross-binary acceptance loop's "Go loads a Rust-created bundle" step: the bundle
//! carries every shape a real session produces — a system message, a user message with an attachment, an
//! assistant message with tool calls and a dialect raw payload, a tool result, and a final assistant with
//! usage. Not a test, not shipped in the binary.

use std::{error::Error, path::PathBuf};

use iota::app::HostDirs;
use iota::provider::ProviderKind;
use iota::provider::model::{
    AssistantBody, Attachment, Body, JsonObject, Message, Raw, RawContent, ToolCall,
};
use iota::provider::usage::Usage;
use iota::session::SessionStore;

fn main() -> Result<(), Box<dyn Error>> {
    let home = std::env::args_os()
        .nth(1)
        .ok_or("usage: mkbundle <home directory>")?;
    let dirs = HostDirs {
        home: Some(PathBuf::from(home)),
        ..HostDirs::default()
    };
    let store = SessionStore::from_dirs(&dirs)?;
    let mut writer = store.create(
        ProviderKind::OpenAi,
        "gpt-probe",
        Some(0.7),
        "http://127.0.0.1:1/v1",
        "/tmp/mkbundle-cwd",
        false,
        "",
    )?;
    writer.update_meta(|meta| {
        "rust-created session".clone_into(&mut meta.title);
        "high".clone_into(&mut meta.effort);
        meta.context_window = 200_000;
    })?;

    let mut arguments = JsonObject::new();
    arguments.insert("q".to_owned(), serde_json::Value::from(1));
    let calls = vec![ToolCall {
        id: "c1".to_owned(),
        name: "f".to_owned(),
        arguments,
    }];
    let raw = RawContent::OpenAi(Raw::from_string(
        r#"{"role":"assistant","content":null}"#.to_owned(),
    )?);

    writer.append_messages(&[
        Message::system("sys"),
        Message {
            attachments: vec![Attachment {
                filename: "a.txt".to_owned(),
                mime_type: "text/plain".to_owned(),
                data: b"hello".to_vec(),
            }],
            ..Message::user("hi")
        },
        Message::assistant_with_calls("", calls.clone(), Some(raw)).with_usage(Some(Usage {
            input: 1000,
            output: 200,
            total: 1200,
            ..Usage::default()
        })),
        Message::tool_result(&calls[0], "ok", false),
        Message::assistant("done")
            .with_reasoning("th".to_owned())
            .with_usage(Some(Usage {
                input: 10,
                output: 5,
                ..Usage::default()
            })),
    ])?;

    // A record for every remaining shape the loader must survive: an interrupted partial.
    writer.append_messages(&[Message {
        content: "cut".to_owned(),
        body: Body::Assistant(AssistantBody {
            interrupted: true,
            ..AssistantBody::default()
        }),
        ..Message::default()
    }])?;

    println!("{}", writer.id());
    Ok(())
}
