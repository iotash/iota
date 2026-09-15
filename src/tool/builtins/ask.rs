//! The `ask` toolset: model-initiated user interaction — `choose` (1–4
//! single/multi select questions with an optional free-text answer) and `confirm` (one
//! yes/no). Pure UX, no side effects, no approval gate.
//!
//! The set is bound to the host's [`Interactor`]: with one (an interactive session) it
//! contributes both tools, without one (`-m`, any headless run) it contributes NOTHING —
//! the model never sees tools it cannot use. Both calls are
//! [`Presentation::Surface`]: they put their own
//! questionnaire in front of the user, so the chat layer keeps them out of the activity
//! panel and records the answers as their own block.

use std::fmt::Write as _;
use std::sync::Arc;

use crate::BoxFuture;
use crate::provider::model::{JsonObject, ToolDef};
use crate::tool::context::RunCtx;
use crate::tool::{
    AskOption, AskQuestion, AskResult, AskSpec, Interactor, Presentation, Tool, ToolEnv,
    ToolOutput, ToolResult,
};
use serde_json::{Value, json};

use crate::tool::args::{bool_arg, str_arg};
use crate::tool::sets::{RawNode, SetError};

/// Max questions one `choose` call may ask.
const ASK_MAX_QUESTIONS: usize = 4;

/// Max runes of a question header — headers are tab chips, not sentences.
const ASK_HEADER_MAX: usize = 16;

/// `env.interactor` None → no tools; Some → `choose` + `confirm`, in
/// that order. Never errors.
pub fn new_ask_set(env: &ToolEnv, _node: Option<&RawNode>) -> Result<Vec<Arc<dyn Tool>>, SetError> {
    let Some(it) = env.interactor.clone() else {
        return Ok(Vec::new());
    };
    Ok(vec![
        Arc::new(ChooseTool {
            it: Arc::clone(&it),
        }),
        Arc::new(ConfirmTool { it }),
    ])
}

/// The `choose` tool: a 1–4 question wizard.
struct ChooseTool {
    it: Arc<dyn Interactor>,
}

/// The `confirm` tool: one yes/no prompt.
struct ConfirmTool {
    it: Arc<dyn Interactor>,
}

impl Tool for ChooseTool {
    fn def(&self) -> ToolDef {
        let schema = json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": ASK_MAX_QUESTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": {
                                "type": "string",
                                "description": "Very short tab label (max ~12 chars), e.g. \"Auth\", \"Library\"",
                            },
                            "question": {
                                "type": "string",
                                "description": "The complete question, one line",
                            },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {"type": "string"},
                                        "description": {"type": "string"},
                                    },
                                    "required": ["label"],
                                },
                            },
                            "multiple": {
                                "type": "boolean",
                                "description": "Allow selecting several options (default false). Required whenever the question wording invites multiple picks.",
                            },
                            "allow_custom": {
                                "type": "boolean",
                                "description": "Offer an \"Other…\" free-text answer (default true)",
                            },
                        },
                        "required": ["header", "question", "options"],
                    },
                },
            },
            "required": ["questions"],
        });
        ToolDef {
            name: "choose".to_owned(),
            description: CHOOSE_DESC.to_owned(),
            input_schema: object_of(schema),
            deferred: false,
        }
    }

    /// A validation failure is a tool error (`is_error`), never a
    /// hard `Err`; a declined wizard is a plain answer the model handles.
    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let spec = match parse_choose_args(args) {
                Ok(s) => s,
                Err(refusal) => return Ok(refusal),
            };
            let res = self.it.ask(cx, spec.clone()).await;
            Ok(ToolOutput::ok(format_choose(&spec, &res)))
        })
    }

    fn presentation(&self) -> Presentation {
        Presentation::Surface
    }
}

impl Tool for ConfirmTool {
    fn def(&self) -> ToolDef {
        let schema = json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The complete yes/no question, one line",
                },
                "yes_label": {
                    "type": "string",
                    "description": "Label for the affirmative choice (default \"Yes\")",
                },
                "no_label": {
                    "type": "string",
                    "description": "Label for the negative choice (default \"No\")",
                },
            },
            "required": ["question"],
        });
        ToolDef {
            name: "confirm".to_owned(),
            description: CONFIRM_DESC.to_owned(),
            input_schema: object_of(schema),
            deferred: false,
        }
    }

    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let question = str_arg(args, "question").trim();
            if question.is_empty() {
                return Ok(ToolOutput::err("confirm: question is required"));
            }
            let yes = non_blank(str_arg(args, "yes_label"), "Yes");
            let no = non_blank(str_arg(args, "no_label"), "No");
            let spec = AskSpec {
                questions: vec![AskQuestion {
                    header: "Confirm".to_owned(),
                    question: question.to_owned(),
                    options: vec![
                        AskOption {
                            label: yes.to_owned(),
                            description: String::new(),
                        },
                        AskOption {
                            label: no.to_owned(),
                            description: String::new(),
                        },
                    ],
                    multiple: false,
                    allow_custom: false,
                }],
            };
            let res = self.it.ask(cx, spec).await;
            if res.declined {
                return Ok(ToolOutput::ok(DECLINED));
            }
            let sel = res
                .answers
                .first()
                .and_then(|a| a.selected.first())
                .map_or("", String::as_str);
            Ok(ToolOutput::ok(format!("The user chose: {sel}")))
        })
    }

    fn presentation(&self) -> Presentation {
        Presentation::Surface
    }
}

/// What a declined wizard tells the model.
const DECLINED: &str = "The user declined to answer.";

/// The `choose` description the model reads (model-facing text: change it only by decision).
const CHOOSE_DESC: &str = "Ask the user to pick between options on an interactive selector. Use ONLY when you are blocked on a decision that is genuinely the user's to make and the choices are enumerable; for open-ended discussion just ask in text. Each question's `header` is a TAB LABEL — keep it under ~12 characters. If a question's wording invites picking several options (\"select all that apply\"), you MUST set `multiple: true` on that question — the selector renders single-select otherwise. Unless allow_custom is false the user can always answer with their own text instead. The user may also decline to answer; proceed sensibly when that happens.";

/// The `confirm` description the model reads (model-facing text: change it only by decision).
const CONFIRM_DESC: &str = "Ask the user a single yes/no question on an interactive prompt. Use ONLY for a decision that is genuinely the user's to make (e.g. consent before something hard to reverse). The user may decline to answer.";

/// `s` when it has non-space content, else `fallback` — the ORIGINAL string is kept, not the
/// trimmed one.
fn non_blank<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.trim().is_empty() { fallback } else { s }
}

/// `serde_json::Value` → the `ToolDef` schema map.
fn object_of(v: Value) -> Option<JsonObject> {
    match v {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Validates the model's arguments into an [`AskSpec`]. The error
/// text IS the tool result, so every message is byte-exact.
fn parse_choose_args(args: &JsonObject) -> Result<AskSpec, ToolOutput> {
    let raw = args.get("questions").and_then(Value::as_array);
    let raw = match raw {
        Some(v) if !v.is_empty() => v,
        _ => {
            return Err(ToolOutput::err(
                "choose: questions must be a non-empty array",
            ));
        }
    };
    if raw.len() > ASK_MAX_QUESTIONS {
        return Err(ToolOutput::err(format!(
            "choose: at most {ASK_MAX_QUESTIONS} questions per call"
        )));
    }
    let mut spec = AskSpec {
        questions: Vec::with_capacity(raw.len()),
    };
    for (i, rq) in raw.iter().enumerate() {
        let Some(m) = rq.as_object() else {
            return Err(ToolOutput::err(format!(
                "choose: questions[{i}] must be an object"
            )));
        };
        let header = str_arg(m, "header");
        let question = str_arg(m, "question");
        if header.trim().is_empty() || question.trim().is_empty() {
            return Err(ToolOutput::err(format!(
                "choose: questions[{i}] needs header and question"
            )));
        }
        // A too-long header is truncated to a tab-chip length, not rejected.
        let header = truncate_header(header);
        let mut q = AskQuestion {
            header: header.trim().to_owned(),
            question: question.trim().to_owned(),
            options: Vec::new(),
            multiple: bool_arg(m, "multiple", false),
            allow_custom: bool_arg(m, "allow_custom", true),
        };
        for ro in m.get("options").and_then(Value::as_array).unwrap_or(&EMPTY) {
            let Some(om) = ro.as_object() else { continue };
            let label = str_arg(om, "label");
            if label.trim().is_empty() {
                continue;
            }
            q.options.push(AskOption {
                label: label.trim().to_owned(),
                description: str_arg(om, "description").trim().to_owned(),
            });
        }
        if q.options.is_empty() {
            return Err(ToolOutput::err(format!(
                "choose: questions[{i}] needs at least one option with a label"
            )));
        }
        spec.questions.push(q);
    }
    Ok(spec)
}

/// The `options` fallback for a missing/mistyped array (a borrowed empty slice).
static EMPTY: Vec<Value> = Vec::new();

/// [`ASK_HEADER_MAX`] runes, the last replaced by `'…'` when it overflows.
fn truncate_header(header: &str) -> String {
    let runes: Vec<char> = header.chars().collect();
    if runes.len() <= ASK_HEADER_MAX {
        return header.to_owned();
    }
    let mut out: String = runes[..ASK_HEADER_MAX - 1].iter().collect();
    out.push('…');
    out
}

/// The model-facing answer sheet: one `"<header>: <answers>"` row
/// per question. Selected options and a custom answer COEXIST on a multi-select — never
/// let one shadow the other.
fn format_choose(spec: &AskSpec, res: &AskResult) -> String {
    if res.declined {
        return DECLINED.to_owned();
    }
    let mut b = String::new();
    for (i, q) in spec.questions.iter().enumerate() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(a) = res.answers.get(i) {
            parts.extend(a.selected.iter().cloned());
            if !a.custom.is_empty() {
                parts.push(format!("{} (custom answer)", a.custom));
            }
        }
        let answer = if parts.is_empty() {
            "(nothing selected)".to_owned()
        } else {
            parts.join(", ")
        };
        let _ = writeln!(b, "{}: {answer}", q.header);
    }
    b.trim_end_matches('\n').to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use crate::BoxFuture;
    use crate::provider::model::JsonObject;
    use crate::tool::context::RunCtx;
    use crate::tool::{AskAnswer, AskResult, AskSpec, Interactor, Presentation, Tool, ToolEnv};
    use serde_json::json;

    use super::{ASK_HEADER_MAX, new_ask_set, parse_choose_args};

    /// Records the spec and plays back a scripted result.
    #[derive(Default)]
    struct FakeInteractor {
        spec: Mutex<AskSpec>,
        res: AskResult,
    }

    impl Interactor for FakeInteractor {
        fn ask<'a>(&'a self, _cx: &'a RunCtx, spec: AskSpec) -> BoxFuture<'a, AskResult> {
            *self.spec.lock().unwrap_or_else(PoisonError::into_inner) = spec;
            Box::pin(std::future::ready(self.res.clone()))
        }
    }

    impl FakeInteractor {
        fn seen(&self) -> AskSpec {
            self.spec
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    fn ask_tools(it: &Arc<FakeInteractor>) -> (Arc<dyn Tool>, Arc<dyn Tool>) {
        let tools = new_ask_set(
            &ToolEnv {
                interactor: Some(Arc::clone(it) as Arc<dyn Interactor>),
                ..ToolEnv::default()
            },
            None,
        )
        .expect("ask set");
        assert_eq!(tools.len(), 2, "the bound set contributes choose + confirm");
        let mut it = tools.into_iter();
        let choose = it.next().expect("choose");
        let confirm = it.next().expect("confirm");
        (choose, confirm)
    }

    fn args(v: serde_json::Value) -> JsonObject {
        match v {
            serde_json::Value::Object(m) => m,
            _ => JsonObject::new(),
        }
    }

    // Without an Interactor the set contributes NOTHING (the model never sees tools it cannot
    // use, -m mode).
    #[test]
    fn test_ask_set_absent_without_interactor() {
        let tools = new_ask_set(&ToolEnv::default(), None).expect("ask set");
        assert!(
            tools.is_empty(),
            "headless runs must advertise no ask tools"
        );
    }

    // Both ask tools present their own surface; the chat layer routes them past the activity
    // panel.
    #[test]
    fn test_ask_tools_present_a_surface() {
        let (choose, confirm) = ask_tools(&Arc::new(FakeInteractor::default()));
        assert_eq!(choose.def().name, "choose");
        assert_eq!(confirm.def().name, "confirm");
        assert_eq!(choose.presentation(), Presentation::Surface);
        assert_eq!(confirm.presentation(), Presentation::Surface);
        // Never batched: a questionnaire is not concurrency-safe.
        assert!(!choose.supports_parallel(None));
        assert!(!confirm.supports_parallel(None));
        // No approval gate: asking a question changes nothing.
        assert!(!choose.requires_approval());
        assert!(!confirm.requires_approval());
    }

    // Defaults applied on the way in, picks AND a custom answer coexisting on the way out.
    #[tokio::test]
    async fn test_choose_parses_and_formats() {
        let it = Arc::new(FakeInteractor {
            spec: Mutex::default(),
            res: AskResult {
                declined: false,
                answers: vec![
                    AskAnswer {
                        selected: vec!["OAuth".to_owned()],
                        custom: String::new(),
                    },
                    AskAnswer {
                        selected: vec!["requests".to_owned(), "httpx".to_owned()],
                        custom: "aiohttp".to_owned(),
                    },
                    AskAnswer {
                        selected: Vec::new(),
                        custom: "use k3s instead".to_owned(),
                    },
                ],
            },
        });
        let (choose, _) = ask_tools(&it);
        let a = args(json!({"questions": [
            {"header": "Auth", "question": "Which auth?",
             "options": [{"label": "OAuth", "description": "standard"}, {"label": "API key"}],
             "allow_custom": false},
            {"header": "Libraries", "question": "Which libraries?",
             "options": [{"label": "requests"}, {"label": "httpx"}],
             "multiple": true},
            {"header": "Deploy", "question": "How to deploy?",
             "options": [{"label": "k8s"}, {"label": "VM"}]},
        ]}));
        let out = choose
            .call(&RunCtx::default(), &a)
            .await
            .expect("no hard error");
        assert!(!out.is_error);

        let spec = it.seen();
        assert_eq!(spec.questions.len(), 3);
        assert!(
            !spec.questions[0].allow_custom && spec.questions[2].allow_custom,
            "allow_custom: explicit false and default true both required"
        );
        assert!(
            spec.questions[1].multiple && !spec.questions[0].multiple,
            "multiple flag mangled"
        );
        assert_eq!(spec.questions[0].options[0].description, "standard");
        assert_eq!(
            out.text,
            "Auth: OAuth\nLibraries: requests, httpx, aiohttp (custom answer)\nDeploy: use k3s instead (custom answer)"
        );
    }

    // Models occasionally serialize booleans as strings; a silent type mismatch must not turn
    // a promised multi-select into a single-select.
    #[test]
    fn test_choose_string_booleans() {
        let spec = parse_choose_args(&args(json!({"questions": [
            {"header": "Pets", "question": "Which pets?",
             "options": [{"label": "cat"}, {"label": "dog"}],
             "multiple": "true", "allow_custom": "false"},
        ]})))
        .expect("parse");
        assert!(spec.questions[0].multiple, "multiple:\"true\" must coerce");
        assert!(
            !spec.questions[0].allow_custom,
            "allow_custom:\"false\" must coerce"
        );
    }

    // Every rejection is a tool ERROR the model can read and retry, never a hard failure.
    #[tokio::test]
    async fn test_choose_validation() {
        let (choose, _) = ask_tools(&Arc::new(FakeInteractor::default()));
        let one = json!({"header": "H", "question": "q", "options": [{"label": "a"}]});
        let cases = [
            (
                "no questions",
                json!({"questions": []}),
                "choose: questions must be a non-empty array",
            ),
            (
                "missing header",
                json!({"questions": [{"question": "q", "options": [{"label": "a"}]}]}),
                "choose: questions[0] needs header and question",
            ),
            (
                "no options",
                json!({"questions": [{"header": "H", "question": "q", "options": []}]}),
                "choose: questions[0] needs at least one option with a label",
            ),
            (
                "five questions",
                json!({"questions": [one, one, one, one, one]}),
                "choose: at most 4 questions per call",
            ),
            (
                "not an object",
                json!({"questions": ["nope"]}),
                "choose: questions[0] must be an object",
            ),
        ];
        for (name, a, want) in cases {
            let out = choose
                .call(&RunCtx::default(), &args(a))
                .await
                .expect("no hard error");
            assert!(out.is_error, "{name}: want a tool error");
            assert_eq!(out.text, want, "{name}");
        }
    }

    // A too-long header is truncated to a tab-chip length, not rejected.
    #[tokio::test]
    async fn test_choose_header_truncated() {
        let it = Arc::new(FakeInteractor {
            spec: Mutex::default(),
            res: AskResult {
                declined: false,
                answers: vec![AskAnswer {
                    selected: vec!["a".to_owned()],
                    custom: String::new(),
                }],
            },
        });
        let (choose, _) = ask_tools(&it);
        let a = args(json!({"questions": [
            {"header": "an unreasonably long tab header", "question": "q",
             "options": [{"label": "a"}, {"label": "b"}]},
        ]}));
        choose.call(&RunCtx::default(), &a).await.expect("call");
        let header = it.seen().questions[0].header.clone();
        assert!(
            header.chars().count() <= ASK_HEADER_MAX,
            "header not truncated: {header:?}"
        );
        assert!(header.ends_with('…'), "truncated header: {header:?}");
    }

    // Declining is an answer, not an error.
    #[tokio::test]
    async fn test_choose_declined() {
        let it = Arc::new(FakeInteractor {
            spec: Mutex::default(),
            res: AskResult {
                declined: true,
                answers: Vec::new(),
            },
        });
        let (choose, _) = ask_tools(&it);
        let a = args(json!({"questions": [
            {"header": "H", "question": "q", "options": [{"label": "a"}, {"label": "b"}]},
        ]}));
        let out = choose.call(&RunCtx::default(), &a).await.expect("call");
        assert!(!out.is_error, "declined must not be an error");
        assert_eq!(out.text, "The user declined to answer.");
    }

    // The yes/no spec shape and its answer text.
    #[tokio::test]
    async fn test_confirm() {
        let it = Arc::new(FakeInteractor {
            spec: Mutex::default(),
            res: AskResult {
                declined: false,
                answers: vec![AskAnswer {
                    selected: vec!["Ship it".to_owned()],
                    custom: String::new(),
                }],
            },
        });
        let (_, confirm) = ask_tools(&it);
        let a =
            args(json!({"question": "Deploy now?", "yes_label": "Ship it", "no_label": "Hold"}));
        let out = confirm.call(&RunCtx::default(), &a).await.expect("call");
        assert!(!out.is_error);
        assert_eq!(out.text, "The user chose: Ship it");
        let q = it.seen().questions[0].clone();
        assert!(!q.allow_custom && !q.multiple);
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[1].label, "Hold");
        assert_eq!(q.header, "Confirm");

        // Defaults + the required-question guard.
        let out = confirm
            .call(&RunCtx::default(), &args(json!({"question": "  "})))
            .await
            .expect("call");
        assert!(out.is_error);
        assert_eq!(out.text, "confirm: question is required");
        confirm
            .call(&RunCtx::default(), &args(json!({"question": "ok?"})))
            .await
            .expect("call");
        let labels: Vec<String> = it.seen().questions[0]
            .options
            .iter()
            .map(|o| o.label.clone())
            .collect();
        assert_eq!(labels, ["Yes".to_owned(), "No".to_owned()]);
    }
}
