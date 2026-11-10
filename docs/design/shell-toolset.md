# The shell toolset — a real shell behind an OS sandbox

The `shell` built-in toolset holds one tool, `shell`: a real shell command
line (pipes, redirects, chaining, heredocs), replacing the earlier argv-only
`run_command` and its program allow-list — with a full shell, argv
allow-listing is meaningless, so containment moved down a layer, to the OS.

## Survey — how the industry sandboxes agent shells

| agent | primitive | writes | network | prompts |
|---|---|---|---|---|
| Claude Code (sandbox-runtime) | macOS Seatbelt / Linux bubblewrap | workspace | localhost proxy + domain allowlist | none inside the sandbox |
| Codex CLI | macOS Seatbelt (`sandbox-exec`) / Linux bwrap + seccomp (Landlock fallback) | read-only / workspace-write modes | off by default | approval escalation |
| Gemini CLI | Seatbelt profiles or Docker/Podman | workspace | optional proxy | `--sandbox` flag |

Adopted consensus: **Seatbelt on macOS, bubblewrap on Linux; writes = project
root + temp; network off by default; sandboxed runs don't prompt, unsandboxed
runs need consent.** The domain-allowlist network proxy (Claude Code's) is
deliberate future work.

## Layering

- **src/shell** — mechanism: execution (`exec.rs`: interpreter lookup,
  timeout, streaming capped buffer + middle-elided output truncation), jobs
  (`jobs.rs`), the interpreter choice (`interp.rs`), the iota exception
  (`selfcall.rs`) and the platform sandbox builders (`sandbox/darwin.rs`
  Seatbelt, `sandbox/linux.rs` bwrap, `sandbox/other.rs` none). Results are
  structured (exit code, timeout, cancellation, setup error) — no
  model-facing wording here.
- **src/tool/builtins/shell.rs** — policy: config decoding, the tool surface
  (def/args), result formatting, and the approval contract.

## Sandbox semantics

Writable roots: the project root, `os.TempDir()`/`/tmp`, the user cache dir
(Go/npm/pip build caches live there — blocking it breaks every build), plus
the top-level `write_roots:` — the same list the code tools' jail admits
(docs/design/code-toolset.md, Config); `~` is home, a relative entry hangs
off the project root. When it lists any, the tool description's sandbox line
names them (`confined to the project root, the extra write roots (…) and
temp/cache directories`); with none the line is unchanged. Network is blocked unless
`network: true`.

What a write outside those roots does depends on whether the sandbox is on:

- **Sandbox on**: the command runs without asking (iota's own invocation
  excepted: it leaves the sandbox and is asked about, Approval below), and
  the OS refuses the write — the command sees an OS error (`Operation not permitted` under
  Seatbelt, `Read-only file system` under bwrap's read-only bind) and its
  exit status says so. Nothing in iota inspects the command's paths.
- **Sandbox off** (`sandbox: off`, or no sandbox on this platform): the call
  is approval-gated (Approval, below) unless `auto_run`, and once allowed it runs with the
  user's full permissions. Its paths are **not checked at all** — the code
  tools' jail is theirs alone, and `write_roots:` changes nothing here.

**Protected paths.** Inside those roots, the sandbox still never writes the
protected table (`app::protected::table`, the one the code writers read —
code-toolset.md, Protected paths): `<root>/.iota.yaml|yml`, `<root>/.git/hooks`,
`<root>/.git/config`, `~/.iota/trusted.json`, and the `<root>/.git` and
`~/.iota` entries themselves (not their contents), so neither can be renamed
or replaced to move the others out from under the rule. A sandboxed command
runs without approval, and without this a silent `echo … >
.git/hooks/pre-commit` ran in the user's own terminal on their next commit.
`write_roots:` cannot re-open them (listing `<root>/.git` opens the rest of
`.git` only). What trips it, and what the command sees — the same OS error as
any denied write (`Operation not permitted` / `Read-only file system`):
`git config …` and `git remote add` (they rewrite `.git/config`), `git init`
in a directory without `.git` (it writes `.git/config`), hook installers
(husky, pre-commit, lefthook), `git submodule add`, `git push -u` (tracking
config). `git add`/`commit`/`checkout`/`fetch` write only objects, refs and
the index, and work. The shell's description says so in one sentence, so the
model asks the user instead of fighting it. Outside the sandbox (`sandbox:
off`, iota's own invocation) nothing is checked, as for any path.

The `cwd` argument is not jailed either: an absolute `cwd` outside the root is
accepted (a bad one fails at spawn), and the sandbox, not the cwd, bounds the
writes.

- **macOS**: `/usr/bin/sandbox-exec` with an allow-default SBPL profile that
  denies `file-write*` outside the writable roots and (optionally)
  `network*`; the last matching rule wins, so targeted allows override the
  broad denies. Paths enter via `-D` parameters, never spliced into the
  profile (quoting stays sandbox-exec's problem). `/tmp`//`/var` symlink
  twins under `/private` are added automatically. After the allow block, one
  `(deny file-write* …)` block closes the protected paths again (`subpath`
  for a tree, `literal` for an entry, params `P0…`), each with its twin and
  its real location. Seatbelt matches paths, not files, so a protected path
  that does not exist yet cannot be created either.
- **Linux**: `bwrap --ro-bind / /` with the writable roots bind-mounted
  read-write on top, `--unshare-net` unless network is enabled,
  `--die-with-parent`. Requires bwrap on PATH and unprivileged user
  namespaces. After every writable bind, each protected path that EXISTS and
  lies under a bound root is bound over itself: `--ro-bind` for a tree (or a
  `gitdir:` file), `--bind` for an entry directory, which keeps its contents
  writable but makes it a mount point that cannot be renamed or removed
  (`EBUSY`). A path that does not exist cannot be bound, so on Linux a
  command can still CREATE a missing `.iota.yaml` or the `.git/hooks` of a
  repository it inits; the code writers' refusal covers that case, the
  sandbox does not.
- **Windows / no bwrap**: no sandbox — see approval below.

## Approval

`shell` implements the same `approver`/`tool.ApprovalReporter` seam as the
code set: a sandboxed call is pre-contained and runs **without prompting**
(the industry default); an unsandboxed call (platform gap or `sandbox: off`)
asks allow once / allow for this session / deny, and is rejected outright in
non-interactive `-m` runs. `auto_run: true` waives approval for unsandboxed
calls.

One call leaves a sandboxed set: iota itself (`shell::selfcall`, DIVERGENCES
X-44). A command whose first word resolves to the running binary — with POSIX
quoting, no pipe, chain or substitution outside single quotes — is spawned
without the sandbox, because writing the config file, opening a browser and
reaching an API from a child agent are what the sandbox refuses. Approval is
the unsandboxed rule for that call alone (`auto_run` waives), the header and
the prompt read `(outside the sandbox)`, and it never rides a parallel batch.

## Config

```yaml
tools:
  shell:               # empty = sandbox auto + network blocked
    # sandbox: off     # disable the sandbox (calls then need approval)
    # network: true    # allow network inside the sandbox
    # auto_run: true   # skip approval for unsandboxed calls (incl. -m)
write_roots: [~/.cargo/registry]  # top level: extra writable roots, shared with the code tools
```

`tools.shell.write` is retired: a config that still writes it gets no shell
set and the warning ``toolset "shell": `write` is gone — list extra writable
roots in the top-level `write_roots:` (shared by the shell and code sets)
(ignored)`` — never a silent drop.

The pre-bash allow-list config shape (`command: [git, ssh]`) is gone with the
`command` set name itself; a list-shaped value fails the mapping decode and
the set is skipped with a warning.

## Execution details

No deadline unless the call's optional `timeout` argument sets one: the
command runs until it exits (or iota exits, which kills every job), and with a
`timeout` it is killed after that many seconds — any positive integer; `0`,
negative or not a number is refused (`timeout must be a positive number of
seconds`) and nothing runs. Earlier cancellation via ESC (the chat layer
cancels the context; sandbox-exec execs bash in-process and bwrap uses
`--die-with-parent`, so the tree dies with the wrapper). A default could only
kill a server, a watcher or a child agent at an arbitrary minute once a long
command is a job rather than a held turn, so there is none (DIVERGENCES X-06,
2026-09-23; the 600 s default and the 3600 ceiling of 2026-09-05 are gone).

`"background": true` takes the command out of the round entirely: the same
spawn, the same sandbox, the same `setpgid` and the same `timeout`, but fd 1
and fd 2 go to `<temp>/iota-jobs/<pid>/<id>.log` and the call returns at once
with the job id, the pid and that path. Sixteen jobs per run. When one ends its
exit status and (capped) output enter the conversation as a **notice** — the
model is told, never asked to poll — and the notice's arrival is the same input
queue the user's own typing uses, so it can wake an idle prompt or land at the
next round boundary of a running turn but never between a tool call and its
result. `Jobs::kill_all` (synchronous `killpg`) runs on every exit path: iota
supervises what it started and cannot supervise across processes, so a job that
must outlive it has to detach itself (DIVERGENCES X-07 … X-09).

A round's consecutive `shell` calls run as ONE concurrent batch
(`BashTool::supports_parallel` is unconditionally true), results and event
rows still in call order — a deliberate break with the "only calls that cannot
change state batch" law, for this set alone (DIVERGENCES X-05). The model
wrote both command lines; `&`/`wait`/`xargs -P` inside a single call were
never gated either, and a child agent dispatched with `iota <agent> -m …` is
pointless when the calls queue.

Output is capped on **both axes**, sized to the industry norms (Claude Code
clips bash output at 30k characters; Codex CLI clips by lines and bytes):
**32 KB** (head 8 KB + tail 22 KB — bytes bound the token cost, the actual
context guard) and **512 lines** (head 128 + tail 384 — keeps many-short-line
output readable), middle elided with markers that tell the model to narrow
via head/tail/grep. Compiler and test failures sit at the end, context at the
start, so the tail budget dominates. The output is also **capped while
streaming** (a head buffer + rolling tail ring, ~52 KB of memory total), so a
command printing gigabytes never grows iota's memory. cwd defaults to
the project root; relative `cwd` arguments resolve against it.

## Deferred (v2+)

Domain-allowlist network proxying (sandbox-runtime style), persistent shell
sessions, Landlock as a bwrap-less Linux fallback,
Windows sandboxing, per-command policy classification (Codex execpolicy
style), and a sandbox-denial → "retry unsandboxed with approval?" escalation
flow.

## Testing

src/shell (unit tests) and tests/tool/shell.rs: shell semantics (pipes/exit codes/cwd), cancellation,
truncation, writable-path derivation, and real sandbox isolation (in-root
write allowed, out-of-root denied, `write_roots:` extras honored, the
protected paths denied while `git commit` works —
`the_sandbox_never_writes_a_protected_path`) with an
environment probe that skips where the sandbox cannot run (e.g. nested
sandboxes). tests/tool/shell.rs: config matrix (sandbox/auto_run →
`requires_approval`), legacy-shape and retired-`write` rejection, the sandbox
line naming `write_roots:` (`the_sandbox_line_names_the_write_roots`), model-facing formatting, registry/Merge integration.
