# Changelog

All notable changes to iota are recorded here. The same notes, rendered, are at
<https://iota.sh/changelog>.

## Unreleased

### Added

- **`/jobs`.** While a background job runs, `/jobs` is a command: it opens a
  list with one row per job — the id, how long it has run, the command — and
  Enter on a row opens the job's page: the command in full, the clock with the
  time it started, the pid, the output file, and the file's last twenty lines.
  Esc on the page goes back to the list, Esc on the list closes it. The row
  leaves the completion list with the last job, so the command exists exactly
  when there is something to show (X-50).
- **The status row shows the running jobs.** One job reads `· job b3 cargo test
  1m12s` at the end of the row, the command in the same cyan as the call's
  header and the seconds walking; several read `· 3 jobs 3m01s`, the oldest's
  clock. The segment is gone once the last job's notice has landed, and an iota
  with no job repaints nothing (X-50).
- **`/jobs` is live.** The list and the page re-read the jobs once a second:
  the clocks walk, a job that ends leaves the list and one that starts joins
  it, the cursor stays on the job it was on, and a page open on a job that
  ends says so where the clock was — `finished: exit 0 after 1m 12s`,
  `finished: killed`, `finished: timed out after …` — with the rest of the page
  as it was (X-50).
- **A Kill tab in `/jobs`.** Tab to `Kill`, Space to check jobs, Enter to kill
  them: the surface closes and each job's notice, `[background job b3
  finished: killed] …`, says what happened, as it does for any job that ends.
  There is no kill key on a job's page — letters on the list are its search
  (X-50).

### Changed

- **A running background job keeps the host at working.** While a job runs and
  the chat is otherwise idle, herdr shows the pane `working`, the terminal's
  progress indicator stays on and cmux reads `Running`, until the job's notice
  has had its turn: the job's result still comes back as a reply, so the chat
  is not done before it — `herdr agent prompt --wait` now waits for the job
  too. An approval prompt or a failed turn shows as before (X-47).
- **A `shell` command has no deadline unless the call sets one.** A call without
  `timeout` runs until the command exits — or iota exits, which kills every
  job — instead of being killed after 600 seconds; a call with `timeout` is
  killed after that many seconds, any number, where 3600 used to be the
  ceiling. Once a long command runs on as a background job rather than holding
  the turn, a default could only cut off a server, a watcher or a child agent
  at an arbitrary minute (X-06).
- **A command is cut the same way wherever it is named.** The finished-job
  notice, the `/jobs` row and the status row's job segment show a command as
  the call's `[shell …]` header did: the first line, ` …` when there are more,
  no more than 64 characters. The notice used to paste the whole command, a
  multi-line script included. The full text is on the job's page in `/jobs`
  (X-50).
- **A `shell` call lets go after 20 seconds.** A command still running then
  carries on as a background job, and the call answers with what it printed so
  far — `Still running after 20s as background job b3 …` — so the model never
  has to guess in advance which commands will take long; the notice with the
  exit status and the output arrives when the job ends, as it always did for
  `background: true`. That switch is for what should not be waited on at all —
  a server, a watcher, a child agent. In the chat, a lone call shows `⎿ still
  running after 20s → background job b3` over the output so far, and a group of
  calls says `· job b3 running`. With 16 jobs already running the call is
  waited for as before and says so (X-50).
- **The banner is a card.** The three facts — `ι> iota` and the version, the
  mode row, the directory — stand inside a rounded frame that hugs the widest
  of them; the half-block wordmark is gone. The dots between the mode row's
  segments are dim like the frame, so the facts stand out. A terminal too
  narrow for the mode row and the frame (38 columns in a plain terminal, more
  when the row names a host) gets the three rows bare; `NO_COLOR` keeps the
  frame and loses the colors (X-46).

### Fixed

- **Less flicker while a reply streams and when a picker opens or closes.** In
  terminals that support synchronized output (Ghostty, kitty, WezTerm,
  Alacritty, iTerm2, Windows Terminal, herdr, tmux 3.7 and newer), iota's screen
  updates now appear all at once instead of line by line. Set
  `IOTA_SYNC_OUTPUT=off` to turn this off (X-55).
- **No line is lost while a reply streams through a resize.** Dragging a tmux
  window's or pane's height while a reply was streaming lost a few of its lines
  every time; so did a height drag in a terminal that never answers iota's
  cursor query, and, rarely, one in tmux under load. iota now draws the whole
  prompt area counting from the cursor rather than from row numbers, so rows the
  terminal moves in the middle of a resize move with it (X-54).
- **Opening or closing a picker no longer blinks the prompt.** A change in the
  prompt area's height used to erase it and draw it again; now only the rows
  that change are written (X-54).
- **Dragging a window's or a tmux pane's height down and back no longer loses
  a line.** In tmux, growing the height again within the same drag pulls rows
  back out of the scrollback; iota could erase the conversation's last line
  above the prompt while it caught up. The resize now reads the terminal's
  size at that moment and confirms where the prompt is before it erases
  anything (X-52).
- **A terminal that doesn't answer iota's cursor query no longer loses a line
  on a resize.** Over a slow ssh link, or in a terminal that ignores the query,
  changing the window's height could erase the conversation's last line; iota
  now erases nothing above the prompt. Resizing in tmux also no longer erases a
  line that tmux moves back onto the screen in the middle of a resize (X-52,
  X-54).
- **Resizing the window no longer throws the prompt to the top or erases
  text.** Making the window narrower moved the prompt and its separators to
  the top of the screen and wiped everything that was on screen above them;
  in a terminal that keeps cleared screens (tmux) each step of a drag left
  another copy of the separators in the scrollback instead. iota now handles
  the resize itself: the prompt stays at the bottom through any drag, the
  transcript above it is intact, and no line, separator or blank row is left
  behind — also with a picker such as `/model` open, while a tool runs, and
  when a maximized window is restored to half its width. While you drag the
  window's edge, the prompt's lines stop a few columns short of the right edge
  (twice the widest step of the drag, at most 8) so the drag does not break
  them; two seconds after the last resize — or at once when you type — they
  span the full width again. A drag can still leave up to two blank or
  repeated lines in the scrollback, never a lost one (X-52). The staged last
  lines of a reply stay where they are, rewrapped for the new width, instead
  of being printed again. A row that ends in a wide character — CJK, an
  emoji — is measured to that character's right edge, so deleting one at the
  end of the prompt never erases half of the one before it (X-52).
- **Narrowing the window with a wide line still staged no longer stops the
  interface** in a debug build; the line is rewrapped for the new width.
- **The status row keeps its colours when it is cut.** A row wider than the
  terminal — the token figures grown a few digits, a long model name, a job
  segment — used to fall back to one faint plain line; it is now cut with its
  hues and its closing reset intact (X-51).
- **The banner no longer prints twice on exit.** Leaving the chat right after
  it started — Ctrl+C or Ctrl+D at the prompt — left the banner on the screen
  twice: once in the scrollback, once more in the frame that had shown it. The
  exit now repaints the frame after the staged rows have gone to scrollback,
  so what stays on the screen is the transcript once, then the prompt and the
  status row. After a longer chat the last rows of the reply were the ones
  doubled; those are once now too (X-49).

## 0.4.0 - 2026-09-22

iota tells its terminal what it is doing, and the config says which model a
chat starts on. Inside a [herdr](https://herdr.dev) pane the chat is listed
from the moment it is up — `working`, `blocked`, `idle`, released on exit —
and the host is named in the harness prompt and on the banner. An agent now
writes `model:` for the model it starts on and `choices:` for what `/model`
and `-M` offer, a breaking change to the config file. The startup banner is a
logo and three facts.

### Added

- **iota reports its state to herdr.** Inside a [herdr](https://herdr.dev)
  pane — `HERDR_ENV=1`, a pane id and the socket herdr injects — iota tells
  herdr what the chat is doing, over that socket, and herdr stops guessing from
  the screen: `idle` from the moment the chat is up, `working` while a turn
  runs, `blocked` at an approval or a question, `idle` again when it is your
  move (after a failed turn too), the session
  the chat writes into (at start-up, after `/save`, after a `/session` switch),
  and a release on exit. A headless `iota run <agent> -m …` in a pane reports
  too: working from the start, idle with the reply, released on exit. Every
  report is bounded and best-effort; nothing herdr does, or fails to do,
  reaches the chat. The notification and the background answer stay the
  terminal's — herdr words its own from the state, and its pane answers OSC 11
  itself. Hosts nest, and iota talks to the innermost one only: a herdr pane
  inside a cmux window reports to herdr and never runs `cmux` (X-47).
- **The harness prompt names the host.** A detected host adds its facts to
  the `<environment>` block, after the run's own: `host: herdr`, `herdr pane:
  w1:p2`, and the workspace and tab when the pane has them; `host: cmux`,
  `cmux surface: <id>` under cmux. A plain terminal adds nothing.
- **The banner names the host.** Inside herdr or cmux the mode row ends with
  `· in herdr` or `· in cmux` — `chat · session 01J… · in herdr`. A plain
  terminal's row is unchanged.

### Changed

- **`agents.<name>.models` is two keys now — a breaking change.** `model:` is
  the model a run starts on (a `models:` entry by name, or `provider:id`);
  `choices:` is what `/model` and `-M` pick from, in the three forms the old
  list took. Leave `model:` out to start in the picker; leave `choices:` out
  and the picker offers every `models:` entry, in the order you declared them.
  A `provider:*` under `model:` is refused (`agents.a.model: "openai:*" is a
  wildcard — put it in choices: and leave model: unset to start in the picker`),
  a `model:` outside the choices warns and runs anyway, and a config that
  still says `models:` on an agent stops with `` agents.a.models: `models` is
  now `choices:` (what /model and -M pick from) plus `model:` (the one the run
  starts on) ``. Headless with neither `-M` nor `model:` now says `no model
  chosen: set agents.<name>.model or pass -M`. `iota list agents` shows each
  agent's model and how many choices it has; `iota list models <agent>` marks
  the one the run starts on with `*`. The starter config writes `model: gpt`
  (X-48).
- **The banner is a logo and three facts.** A chat opens on three rows: the
  `iota` wordmark on the left and, beside it, the version, the mode row —
  `agent` or `chat`, then `session <id>`, `resumed <id>` or `not saved · /save
  keeps it` — and the directory the chat runs in (the project root in agent
  mode, else the current directory, `~` for home). The `Chat started`,
  `Commands:`, `Session:` and `Agent mode:` rows are gone: the commands are one
  `/` away in the composer, the model is on the status row. A directory
  longer than the row is cut in the middle (`/Volumes/build/…/packages/cli`)
  rather than wrapped; a terminal too narrow for the wordmark, the gutter and
  the mode row (57 columns in a plain terminal, more when the row names a
  host) gets the three facts alone; `NO_COLOR` gets them bare (X-46).

## 0.3.2 - 2026-09-21

The model knows where it is. Every agent with tools now sends a built-in
harness paragraph ahead of its own prompt — who it runs inside, what the
machine looks like, and how iota's own command line is driven — and `iota`
itself runs outside the shell sandbox, so a child agent or an `iota mcp add`
from the chat just works. A first run writes the starter config; `add --url`
logs in on the spot; `/mcp` leaves the chat; the reference moves to iota.sh.

### Added

- **A built-in harness prompt.** An agent with `tools:` sends a short paragraph
  of iota's own ahead of its `system:` — two sentences of identity and two rules
  (a declined tool call is not retried; a command the sandbox refused is reported
  as such, not rewritten), an `<environment>` block (project root, platform,
  shell, date, the `iota` binary, the user and project config files or the `-c`
  file, a missing one marked `(absent)`), and with the `shell` set an
  `<iota_cli>` block: that `iota` itself runs outside the sandbox, its verbs
  (`mcp add|list|get|remove|login|logout`, `config check|path|init`, `list …`,
  `run <agent> -m`), and the three rules — MCP servers through `iota mcp`,
  everything else by editing the config file and `iota config check`, every
  change from the next session. Under 1.5 KB; your prompt follows inside
  `<instructions>`, the AGENTS.md overlay after it. Composed at send time like
  the overlay and never stored: a resumed session and an upgraded binary get the
  current one, and `/model`'s System tab shows the prompt exactly as sent. An
  agent without `tools:` sends nothing extra. No configuration key (X-43).

### Changed

- **The README is an introduction.** The reference — every config key, the
  system prompt in full, every tool parameter, MCP, agent mode, slash commands —
  lives at <https://iota.sh/docs>; the README keeps what a first look needs, and
  the two config errors that pointed at a README section point at the site.
  How a release is cut moved to `docs/RELEASING.md`.
- **A first run writes the starter config itself.** With no `-c` and no
  `.iota.yaml` in the home or the project, `iota` (and `iota run …`) writes
  `~/.iota.yaml` — the file `iota config init` writes — names it on stderr, and
  goes on: `OPENAI_API_KEY=… iota` is a working first run on a fresh install,
  and without the key the key error says what to set. Nothing is written when a
  config exists in either spelling, when `-c` names one, or without a home.
  The two refusals that used to send you to `config init` now say where an
  `agents:` entry goes instead — a run without a config no longer reaches them.
- **`iota` runs outside the shell sandbox.** A `shell` call whose first word is
  the running binary — `iota mcp add …`, `iota mcp login <name>`, a child agent's
  `iota run <agent> -m "<task>"` — is spawned without the sandbox, so it can write
  the config file, open the browser and reach an API. Only that shape leaves:
  `iota` in a pipe, a chain, a `$(…)` or a backtick stays in. Approval is as it
  was — a sandboxed set without `auto_run` asks about such a call, the prompt and
  the call header marked `(outside the sandbox)`; `auto_run: true` asks nothing.
  A parent agent no longer needs `network: true` to dispatch a child: the child
  is isolated by its own agent's configuration (X-44).
- **`add --url` logs in on the spot.** Once the entry is written, `iota mcp add
  <name> --url <url>` probes the endpoint with one bare `initialize`: a 401
  starts the OAuth login right there — the same steps as `iota mcp login`, with
  `--no-browser` printing the URL instead of opening the browser — a server that
  answers without a credential is left at `Added`, and one that could not be
  reached keeps the `iota mcp login <name>` hint. `--auth oauth` or `--client-id`
  skips the probe and logs in at once; `--auth none`, an entry with its own
  `Authorization` header, and the new `--no-login` (a script, CI, a machine
  without a desktop) write the entry and stop. A login that fails keeps the
  entry, prints `Retry with: iota mcp login <name>` and exits non-zero. `login`
  stays for logging in again, or for the login `--no-login` skipped (X-41).

### Removed

- **`/mcp` leaves the chat.** The `/mcp` panel and `/mcp login|logout <name>`
  of 0.3.1 are gone. The MCP tab of `/tools` already shows every server's
  state — a server waiting for a login reads `not logged in: run iota mcp
  login <name>` — and the login is `iota mcp login <name>` from a shell (a
  chat already running does not pick the token up; start it again). The
  startup notice names that command now, and the 80-column banner's command
  row fits on one line again. No management command is planned for the chat:
  configuration is headed for a toolset the model calls on your behalf, not
  for slash commands (X-42).

## 0.3.1 - 2026-09-20

MCP servers are managed from the command line and logged in to. `iota mcp
add|list|get|remove` edits the `mcp_servers:` block of one file; a server behind
OAuth 2.1 is one `iota mcp login <name>` signs in to — the client identified
three ways, the consent prompt asked for, `auth` discovered from the server's
own 401 — the shape one Logto tenant taught, a finding at a time (X-37 to X-40
in `docs/DIVERGENCES.md`).

### Added

- **`iota mcp` manages the `mcp_servers:` block.** `iota mcp add <name> -- <command>
  [args…]` or `add <name> --url <url> [--header 'K: V']… [--auth oauth|none]`,
  `list [--scope user|project|all] [--json] [--probe]`, `get <name>` and
  `remove <name> [--scope]`. Two scopes and no third file: `--scope user`
  (`~/.iota.yaml`, the default) or `--scope project` (`./.iota.yaml`, where a
  header or environment value must be a `${…}` reference), or the `-c` file
  alone. The block is machine-managed — every other byte of the file stays as you
  wrote it; a comment inside the block is not kept.
- **MCP OAuth 2.1.** A server that asks for a login is one you log in to: `iota mcp
  login <name>` discovers the authorization server, registers a client when the
  server offers it, opens the browser (`$BROWSER`, else the platform opener; the
  URL is printed either way, and a pasted redirect URL works without a browser)
  and stores the tokens in `~/.iota/mcp/auth/<name>.json` (mode 0600). A run puts
  the bearer token on every request and refreshes it when the server rejects it;
  a server with no usable token is reported as `not logged in: run iota mcp login
  <name>` and left out of that run alone, and a stored login the server will not
  refresh is reported with the server's answer and the same `login` to run.
  `iota mcp logout <name>` revokes and forgets. In the chat, `/mcp` shows every
  server's login state, `/mcp login <name>` runs the same flow and reconnects the
  server, `/mcp logout <name>` takes it down.
- **A login identifies iota three ways.** The entry's `client_id` (a client
  registered out of band — `iota mcp add … --client-id <id>
  --client-secret-env VAR`, the secret only ever a `${env:VAR}` reference; or
  `--client-id` on `login`), else dynamic registration when the server offers it,
  else iota's Client ID Metadata Document (`https://iota.sh/oauth/client.json`)
  when the server accepts one — the shape of an authorization server like Logto,
  which registers nobody. A login asks for the scopes the resource names, plus
  `offline_access` when the server lists it, and carries the RFC 8707 `resource`.
- **A pre-registered client listens on a fixed port.** Its redirect URI must match
  what was registered, so `login` binds `127.0.0.1:17801` (or the entry's
  `redirect_port`, `--redirect-port` on `add`) for it and reports a taken port
  instead of moving; `iota mcp get`/`list` show the `redirect_uri` to register,
  and `login` prints it as `Redirect:`. A refusal names the server's
  `error_description` and `error_uri`, logs the whole callback under `IOTA_LOG`,
  and — for a bare `access_denied` against the metadata document — says what a
  Logto tenant does about it.
- **The authorization request asks for the consent prompt.** Every `login` sends
  `prompt=consent` (OpenID Connect Core §3.1.2.1), as Claude Code does; a server
  that does not know the parameter ignores it (RFC 6749 §3.1). A Logto tenant
  answered a request without it with a bare `access_denied` right after the
  consent page — the whole of why `iota mcp login` failed against namebeta while
  Claude Code worked.
- **`auth` is discovered.** An HTTP server no longer has to be declared `auth:
  oauth` to be logged in to. Not written (the default) is `auto`: a run connects
  bare, and a 401 at the handshake is the server asking for a login — that
  server is reported as `not logged in: run iota mcp login <name>` and left out of
  the run, not "connect failed"; once a token file exists for its name, every
  later connect is an OAuth one. `auth: oauth` forces the login (no bare
  attempt), `auth: none` forbids it (a 401 is a failed connect; `login` refuses the
  entry), and an entry that writes its own `Authorization` header counts as
  `none` — that credential is the one to fix, as Claude Code has it. `iota mcp
  add --url` writes no `auth:` unless `--auth oauth|none` says so, and
  `--client-id`/`--client-secret-env`/`--redirect-port` no longer need `--auth
  oauth`; the line after an add is `Next: iota mcp login <name>` when the entry
  says there is a login, `if the server asks for a login: …` when it does not
  say. `list`/`get` show `auto` (`auto: logged in` with a token file), `--probe`
  labels a server that asked `needs login: iota mcp login <name>`, and `login`
  against a server that neither challenges nor publishes protected-resource
  metadata says it "does not ask for a login".

## 0.3.0 - 2026-09-18

The tree is native now. Nineteen refactoring steps took the port from a
module-by-module translation to a Rust shape of its own — a layered module tree
that a test pins, one editing core, typed errors instead of strings, an enum
where a bag of flags used to be — with every step green on all three platforms
and every user-visible text byte-identical. What you can see is below; the rest
is in `docs/ARCHITECTURE.md`.

### Changed

- **The two rules around the composer are dashed.** They were solid (`─`), a wall
  across the conversation; they are now a light triple dash (`┄`), a strip the
  input sits in. Nothing else about the frame moves.
- **Ctrl+W in a surface field follows the composer's rule.** The `/model` combo
  field, `/file`'s path field, the Ask "Other…" editor and the search query used
  to cut back to the last *space* — `a<Tab>b` and Ctrl+W emptied the field. All
  of them now edit through the one editor the composer uses: trailing whitespace
  is skipped, then the previous word goes, any Unicode whitespace is a boundary,
  and the line start is never crossed.
- **`iota list providers` names the key source a run would actually use.** It said
  `[key: config]` whenever `key:` was set, even with the type's environment
  variable set too — and a run takes the variable first. The listing now follows
  the one precedence: the variable when set, else `key:`, else nothing.

### Fixed

- **A finished background job's notice lands at a round boundary.** Drained
  mid-turn, the `[background job … finished]` line was printed above the rows of
  the call that was running when the job ended; it now settles that activity
  group first and lands after it, exactly where a queued message would.
- **A new skill directory is noticed on Windows.** The skills catalog re-derived
  itself on the root directory's mtime alone, which NTFS does not always move in
  time; the probe now lists the root's entries as well, so a skill that appears or
  goes changes the list itself.

### Internal

- The module tree is layered and `tests/layering.rs` pins the order: `app ← text
  ← llm ← provider ← tool ← {shell, agents, mcp} ← headless`, `config ← {tool,
  session}`, `markdown ← text`, `ui ← markdown`, `repl ← ui`. The ledger of what
  moved is `docs/ARCHITECTURE.md` §2; `docs/ROADMAP.md` §3 records the phase.
- `Result<_, String>` is gone (22 places) and every lock rides one helper
  (`sync::lock`; the inline `PoisonError` handlings went from 121 to the four that
  are the helper itself). The `CliError` is three stages, `Repl` is three
  structs and a `TurnEngine`, the markdown `Writer` is a state machine over an
  `enum Block`, and `shell`, `mcp` and `tool` report outcomes as enums.
- The real-terminal suite grew from 16 to 22 tmux scenarios (505 checks): the
  title stack, the host channels, background jobs, block previews, an emoji table
  and the retry path are pinned, and `docs/TUI-VERIFY.md` says which items only a
  human at a terminal can still judge.

## 0.2.1 - 2026-09-15

A fix release. Three of the fixes are about a token count that read 0, or a
request that read 400, against endpoints that speak a dialect *almost* right.
They were found the same way: one prompt sent through every provider in a real
config, and the bytes on the wire read back.

### Added

- **`/model` offers the agent's candidate set.** The picker used to ask the one
  provider the run had resolved to and fork on the answer — a list when the
  listing worked, a bare name field when it did not. It now lists what
  `agents.<name>.models` declares: entries and `provider:id`s as written, and
  every `provider:*` wildcard fetched in one concurrent round (the wait is the
  slowest endpoint, not the sum; Esc drops them all). A wildcard whose listing
  fails is one row saying so, not a surface that gives up. The list is a combo
  box: the query field is always open, filters as you type, and its text
  commits as typed through a `use "…" as typed` row after the last real one.
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

### Changed

- **`-M provider:id` brings the `models:` entry serving that pair, or nothing
  — never the first candidate's knobs.** `-M` used to replace the model id and
  leave every other field of the entry the run had resolved to first in place,
  so `iota run chat -M claude:claude-haiku-4-5-20251001` on an agent whose
  first candidate declares `temperature` and `top_p` sent both to Claude, which
  refuses the pair: a 400 for a flag that named a different model. The flag now
  resolves to the entry that serves exactly that provider and id when there is
  one — its own knobs travel with it — and to the bare pair otherwise, every
  other field at its default; the agent's overrides apply as before. A bare
  `-M id` is `<current provider>:id` and follows the same rule. `-M ""`,
  `provider:*` and entry names are unchanged.
- **`effort: xhigh` and `effort: max` reach Gemini as `thinkingLevel: HIGH`.**
  Gemini 3 knows LOW, MEDIUM and HIGH; the value used to go up uppercased and
  verbatim, so an agent with `effort: max` got a 400 from Vertex on every call.
  OpenAI and Anthropic still receive the value as written.
- **rustls is 0.23.45** (RUSTSEC-2026-0285), and `cargo deny` now gates the
  dependency graph in CI: licenses, advisories, duplicate versions, sources.

### Fixed

- **Input tokens no longer read 0 on Anthropic-compatible endpoints that report
  them late.** The Anthropic stream carries the input count in `message_start`
  and the output count in `message_delta`; GLM's endpoint (open.bigmodel.cn)
  sends `message_start` with zeros and the whole figure in `message_delta`.
  `message_delta`'s usage is now laid over `message_start`'s, so whichever
  event carries a number wins. The official API reports the same numbers as
  before.
- **A usage object carrying both OpenAI namings decodes.** zenmux reports
  `input_tokens` beside `prompt_tokens`, `output_tokens` beside
  `completion_tokens` and both `*_tokens_details` — every figure under both
  names. The decoder treated the pair as a duplicate field and dropped the
  event it rode in: in a stream that is the final event, so the reply arrived
  and the usage read 0 with no error; on a `-m` run the whole call failed with
  `chat error: duplicate field 'input_tokens'`. The object is now read name by
  name, the Responses name first.
- **An MCP tool that declares no input goes out without an empty schema on the
  Responses wire.** `"parameters":{}` — a schema with no `type` — was still
  sent on `/responses` where chat-completions had already dropped it.
- **The MCP handshake reports iota's real version.** `clientInfo.version` had
  been the literal `1.0.0` since the port began.
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
