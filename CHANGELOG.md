# Changelog

All notable changes to iota are recorded here. The same notes, rendered, are at
<https://iota.sh/changelog>.

## Unreleased

### Added

- **`NO_COLOR` is honoured — and so are `TERM=dumb` and a piped stdout.** None of
  the three did anything before: every style was hard-wired on, so a terminal
  that asked for no color still got bold, faint and underline, and a bare reset
  where each color had been. Now the decision is made once at startup and the
  chat — replies, tool output, diffs, links — is plain text with no escape
  sequence at all, while the frame around it (composer, status row, panels)
  keeps its bold, faint and reverse video and drops every color, so it stays
  readable. Images still render in color; their pixels are the picture.
- **`IOTA_LOG=<path>` writes iota's internal diagnostics to a file** — its own
  events at debug level, its libraries' at info, one timestamped line each. It
  is a developer's tap: nothing you need to see depends on it.

### Fixed

- **An MCP server that lists the same tool twice now says so.** The duplicate
  was skipped silently: the warning went to a log channel nothing listened to,
  and the release build had compiled it out entirely. It is now one
  `Warning: mcp server <name>: duplicate wire tool name <wire>, skipping` line
  on stderr for a `-m` run, and a `⚠ MCP <name>: …` notice in the chat.

- **`edit_file` no longer corrupts files that are not UTF-8.** It read the whole
  file through a lossy decode and wrote the decoded result back, so one edit to
  a source file in Latin-1, Shift-JIS or GBK replaced every byte in it that is
  not valid UTF-8 with `U+FFFD` — across the whole file, not only the span
  being edited — and the diff you were shown came from the same decoded buffer,
  so it looked like nothing had happened. The search, the uniqueness check and
  the replacement now run on the file's bytes and the original bytes are
  written back; only the diff and the numbered snippet are still text, because
  a terminal renders text. **This bug is in 0.1.0 and 0.2.0**: a file one of
  those versions edited may have lost its non-ASCII content, and the loss is
  not recoverable from the diff — check it against version control.

## 0.2.0 - 2026-09-14

Four parameters — `context_window`, `effort`, `temperature` and `top_p` — were
read once, when a session started, and never looked at again. They now resolve
from layers, at every moment that can change the answer. The rule is one
sentence: when the config speaks the config decides; when the config is silent
your hand-set value stands.

### What changes

- **`/model` re-evaluates the four parameters.** Switching model used to move
  the provider's model id and the id in the session meta, and nothing else: a
  chat that went from a 400k model to a 128k one kept counting against 400k,
  and an `effort` inherited from the entry it left followed it to a model that
  had never asked for one. The switch now asks the model it arrives at what it
  declares, applies whatever moved to the provider and the token budget, and
  says so one notice at a time. Choosing the FIRST model does the same — a run
  that started on a `provider:*` wildcard had no entry to read them from until
  then.
- **A value the chat inherited is dropped on a switch; one you set by hand is
  kept.** Which is the same rule from the session's side. A window that came
  from a declaration gives way to the built-in default on a model that declares
  none, rather than silently following you to a model half its size; a window
  you typed in `/model` survives. Adjusting a knob there is the answer for what
  no declaration covers, not a permanent override of one — to change a value
  for good, write it into the config.
- **`defer_mode` is not one of the four.** The deferring wrapper is built once,
  around the MCP dispatcher, and there is no seam to rebuild it mid-chat, so
  switching to a model that asks for another mode prints a note that this
  session keeps the one it started under. A `context_window:` on the model
  being switched to that does not parse is a warning for the same reason: the
  chat is already running, and the honest answer to a bad value is to leave
  that key silent and say so.

### What is new

- **`agents.<name>.context_window`.** The same key as the model's, parsed the
  same way, and the top layer: an agent that knows how long its conversations
  run says so once instead of forking a `models:` entry per usage.
- **Three tiers, two evaluating moments.** `agents:` first, then what the model
  the chat is RUNNING declares, then — lowest — the value the session is
  already running under. A new session and a model switch are the only two
  moments that evaluate them.
- **A resume is not one of them.** `iota resume` restores the bundle's own
  values and the origin of each, so an old session continues exactly as it was
  and a config edited in between reaches it the first time you switch models
  inside it. Only a parameter the bundle never recorded is evaluated.

### On disk

- `meta.json` gains `top_p`, which now replays beside effort and temperature as
  it could not while nothing wrote it down, and a record of where each of the
  four values came from — a config declaration, your own hand, or the built-in
  default. Both keys are omitted when they have nothing to say, so a bundle
  written by 0.1.0 keeps its byte shape and still resumes; its silence is read
  as *every value here is the user's own*, the reading that cannot lose
  something a person chose.

### Upgrading

Nothing to edit: a 0.1.0 config loads unchanged and a 0.1.0 session resumes.
The one habit worth revisiting is a knob set in `/model` and relied on to
outlast a model switch — it now yields to a model or an agent that declares
that parameter. If it should always hold, it belongs in `models:` or `agents:`,
and [the config file](https://iota.sh/docs/config-file) has the table.

## 0.1.0 - 2026-09-13

The first tagged release of iota as a Rust binary. It is an agent CLI: you
configure agents — a model, a prompt, a set of tools — and run them. If you
used the Go build, this is the list of things that changed under you.

### What breaks

- **The positional argument is a command, not a name.** `run`, `list`,
  `resume`, `config` and `version` are the whole set, and a bare `iota` means
  `iota run`. `iota <name>` used to be looked up in four namespaces — agents,
  models, providers, then the built-in provider types — so one word could mean
  four things. Now `iota run <agent>` resolves `agents:` alone, and a model or
  a provider is reached through an agent. `--help` is the complete map, and a
  command added later can never shadow an agent you named.
- **Six flags are gone.** `-k/--key`, `-u/--url`, `-t/--temperature`,
  `-S/--system-input`, `--context-window` and the boolean `--agent` are
  refused by the parser. A flag stays on the command line only if it describes
  THIS call: the key comes from an environment variable or
  `providers.<name>.key`, the URL from `providers.<name>.url`, the temperature
  and the window from `models:` or `agents:`, the prompt from
  `agents.<name>.system`, and agent mode from `agents.<name>.workspace`. Nine
  flags remain, and only `-c/--config` is global.
- **`--resume` and `-l` became commands.** `iota resume [<id>]` takes any
  unique id prefix and opens a picker with no id;
  `iota list [agents|models|providers|sessions]` reads the config and the
  session store — no key, no network.
- **The config has three layers, and they are enforced.** `providers:` says how
  to reach an API, `models:` which model and what its protocol looks like,
  `agents:` how it is driven. Every key is audited against the layer it was
  written in before the document is decoded: a key of another layer, a retired
  key, an unknown key, an unknown top-level key and an unknown toolset are all
  errors naming the coordinate and the file. There is no migration layer — a
  one-layer `providers.<name>` block carrying `model:`, `system:`, `tools:` or
  `agent:` is refused, not split.
- **The `delegate` toolset is retired.** A child agent is now
  `iota run <agent> -m "<task>"` run from `bash` — a full run of that `agents:`
  entry, with its own model, tools and session.
- **`defer_mode` moved to `models:`.** The provider — and so the dialect — is
  already fixed there, so a mode the provider cannot speak is refused when the
  config loads instead of silently degrading to `normal`.

### What is new

- **`iota config`.** `init` writes a commented starter config (refusing to
  overwrite one), `path` prints the files a run reads, and `check` loads them
  and reports the three layers, warning when no `agents.default` exists. The
  zero-config start was given up on purpose; `init` is where it went.
- **Headless resume replays the session.** `iota resume <id> -m "…"` takes the
  model (when the session was recorded under the same provider type),
  temperature, reasoning effort, context window and image settings from the
  bundle; an explicit `-M` still wins. The banner goes to stderr, so stdout
  stays the reply — or the JSON report — alone, and the new turn is appended
  only when it succeeds.
- **Dedicated image providers.** `type: imagen` and `type: images` carry models
  that only generate pictures, with `/edit`, `/redo`, reference images through
  `/file`, and per-dialect knobs on `/model`.
- **Background bash jobs.** `"background": true` returns a job id at once and
  the result arrives later as a notice the model is handed. Up to 16 at a time,
  and they are killed when iota exits — a resumed session never inherits one.
- **`iota version`.** The Go build had no way to say its version; `iota version`
  and `--version` both print it.
- **Windows.** `x86_64-pc-windows-msvc` is a release target, installed by one
  PowerShell line. There is no embedded interpreter: the `shell` tool drives
  Git Bash, PowerShell or `cmd.exe` — whichever the machine has — and says
  which one in its own description. There is no OS sandbox on Windows, so
  every command asks, and the binaries are not code-signed; see
  [installation](https://iota.sh/docs/install).

### Fixes worth naming

- `--mcp ""` and `-m ""` are errors with a message instead of a panic or a
  silent fall-through into the TUI.
- A stream chunk whose `error` field is literally `null` is treated as absent,
  not as an in-band error.
- Sparse tool-call indices in a chat-completions stream no longer drop the
  calls after the gap.
- Ctrl+C closes the MCP servers and exits 130; a `--output-format json` run
  prints its report with `"error": "interrupted"`.

### Upgrading

Start from `iota config init` and move what you had into the three layers: the
endpoint into `providers:`, the model into `models:`, the prompt and tools into
`agents:`. `iota config check` names anything still in the wrong place, with
the file and the coordinate — see [the config file](https://iota.sh/docs/config-file).
