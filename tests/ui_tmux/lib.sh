#!/usr/bin/env bash
# L4 tmux harness — shared helpers (TUI_TEST_PLAN §L4).
#
# Ported from the ratatui inline spike's tmux-check.sh (tag go-final: rust/spikes/ratatui-inline), which is
# the proven shape on this host (tmux 3.7c): a PRIVATE tmux server per scenario so the
# developer's own tmux is never touched, `send-keys -l` for literal text, named keys for
# control, `load-buffer` + `paste-buffer -p` for TRUE bracketed paste, `capture-pane` for
# observation and `resize-window` for a real SIGWINCH.
#
# Discipline (TUI_TEST_PLAN §L4): never a fixed sleep where a poll will do —
# `wait_vis`/`wait_all`/`wait_all_more`/`wait_gone` poll for a predicate and `settle` polls
# for three consecutive identical captures (a window wider than one spinner tick). Spinner
# glyphs and elapsed clocks are normalised by `norm` before any shape comparison.
# Assertions prefer semantic invariants (a line present exactly once, a constant cursor
# row, an intact frame) over full snapshots.
#
# The runner (`tests/tmux.rs`) exports: TMUX_BIN, TMUX_SOCKET, IOTA_BIN, IOTA_PORT,
# SCEN_TMP. Each scenario sources this file, calls `start`, asserts with `ok`/`bad`, and
# ends with `finish` (exit 1 if anything failed).

set -u

: "${TMUX_BIN:=tmux}"
: "${TMUX_SOCKET:=iota-test-$$}"
: "${IOTA_BIN:?IOTA_BIN must point at the built iota binary}"
: "${IOTA_PORT:?IOTA_PORT must be the mock provider port}"
: "${SCEN_TMP:=/tmp/iota-tmux-$$}"

PASS=0
FAIL=0
WARTS=0
SCEN_HOME="$SCEN_TMP/home"
mkdir -p "$SCEN_HOME"

# ---------------------------------------------------------------- tmux plumbing

tm() { "$TMUX_BIN" -L "$TMUX_SOCKET" "$@"; }

cleanup() { tm kill-server >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Visible grid, escapes stripped (so assertions read the glyphs the user sees).
cap() { tm capture-pane -pt s 2>/dev/null; }
# Visible grid plus scrollback — the only proof that history was preserved.
capall() { tm capture-pane -pt s -S -400 2>/dev/null; }

type_() { tm send-keys -t s -l "$1"; }
key() { tm send-keys -t s "$@"; }

# write_iota_config <provider> <model> — the pane's config file. The endpoint, the model and the
# agent a run names all live in `$SCEN_HOME/.iota.yaml` now: `-k`, `-u` and the positional
# provider name were retired with the agent-first surface, so a scenario points at the mock the
# way a user points at an endpoint.
# The wildcard beside the entry is what keeps `/model` offering the mock's own listing: the picker
# lists the agent's CANDIDATE SET, so an agent naming one model has one row (plus the combo's input
# row). Scenario 03 picks a second model out of that list. Keep the document free of backticks —
# the heredoc is unquoted (it interpolates $1/$2/$IOTA_PORT), so a backtick would run a command.
#
# A scenario whose subject IS the config (the candidate set behind /model) pre-sets $CONFIG_BODY
# and gets that document instead, with $IOTA_PORT expanded in it so it can point at the mock too.
write_iota_config() {
    if [ -n "${CONFIG_BODY:-}" ]; then
        printf '%s\n' "$CONFIG_BODY" >"$SCEN_HOME/.iota.yaml"
        return
    fi
    cat >"$SCEN_HOME/.iota.yaml" <<EOF
providers:
  mock: {type: $1, key: test, url: "http://127.0.0.1:$IOTA_PORT"}
models:
  m: mock:$2
agents:
  default: {models: [m, "mock:*"]}
EOF
}

# iota_cmd <provider> <model> [extra iota args…] — the command line a pane runs, with its config
# written first. HOME is redirected into the scenario's temp dir so neither the session store nor
# the config ever touches the developer's own.
iota_cmd() {
    local kind="$1" model="$2"
    shift 2
    write_iota_config "$kind" "$model"
    echo "env HOME=$SCEN_HOME $IOTA_BIN $*"
}

# _launch <cwd|""> <provider> <model> <width> <height> [extra iota args…] — a fresh private
# server running the real binary as the SESSION COMMAND. An empty <cwd> inherits the
# runner's directory (what scenarios 01-10 have always had); a non-empty one is passed to
# `new-session -c`, which is how a scenario that writes files (/export writes into cwd)
# keeps them inside its own scratch directory.
_launch() {
    local cwd="$1" kind="$2" model="$3" w="$4" h="$5"
    shift 5
    tm kill-server >/dev/null 2>&1 || true
    _poll_until 20 'server_gone'
    local cmd
    cmd="$(iota_cmd "$kind" "$model" "$@")"
    if [ -n "$cwd" ]; then
        tm new-session -d -s s -x "$w" -y "$h" -c "$cwd" "$cmd"
    else
        tm new-session -d -s s -x "$w" -y "$h" "$cmd"
    fi || {
        bad "tmux session start"
        return 1
    }
    wait_vis '❯' || {
        bad "startup composer never appeared"
        return 1
    }
    return 0
}

# start [width] [height] [extra iota args…] — the openai mock, runner's cwd (scenarios 01-10).
start() {
    local w="${1:-80}" h="${2:-24}"
    shift 2 2>/dev/null || true
    _launch "" openai fake "$w" "$h" "$@"
}

# start_provider <provider> [width] [height] [extra iota args…] — like `start`, but the
# provider is chosen (`images` brings its own default model) and the pane's cwd is
# $SCEN_TMP, so anything the binary writes relative to cwd lands in the scratch directory.
start_provider() {
    local kind="${1:-openai}" w="${2:-80}" h="${3:-24}"
    shift 3 2>/dev/null || true
    local model=fake
    [ "$kind" = "images" ] && model=gpt-image-1
    _launch "$SCEN_TMP" "$kind" "$model" "$w" "$h" "$@"
}

# start_shell [width] [height] — a pane running a bare `sh`, cwd $SCEN_TMP.
#
# `start` launches the binary AS the session command, so a `pipe-pane` attached afterwards
# races the startup bytes (mode 1004, the first frame). A scenario that must observe those
# bytes starts a shell, attaches the pipe, and only then types the command.
start_shell() {
    local w="${1:-80}" h="${2:-24}"
    tm kill-server >/dev/null 2>&1 || true
    _poll_until 20 'server_gone'
    tm new-session -d -s s -x "$w" -y "$h" -c "$SCEN_TMP" "sh" || {
        bad "tmux shell start"
        return 1
    }
    _poll_until 30 alive || {
        bad "the shell pane never came up"
        return 1
    }
    return 0
}

# ---------------------------------------------------------------- raw byte capture
#
# `capture-pane` hands back the RENDERED grid, so a control sequence the app writes for the
# terminal itself (OSC 9;4 progress, OSC 9 notify, mode 1004) is invisible to it. `pipe-pane`
# taps the pane's output BYTES instead, which is the only way to assert them.

RAW="$SCEN_TMP/raw.out"

# Starts (or restarts) the raw tap. Call it BEFORE the binary starts — see `start_shell`.
pipe_raw() {
    : >"$RAW"
    tm pipe-pane -t s -o "cat >> $RAW"
}

# raw_has <fixed string> — the bytes captured so far contain it (binary-safe).
raw_has() { LC_ALL=C grep -qaF -- "$1" "$RAW" 2>/dev/null; }
# raw_lacks / wait_raw — the polling twins of the two assertions a scenario makes.
raw_lacks() { ! raw_has "$1"; }
wait_raw() { _poll_until 120 raw_has "$1"; }

# check_raw <description> <fixed string> <yes|no> — present / absent in the raw capture.
check_raw() {
    if [ "$3" = "yes" ]; then
        if raw_has "$2"; then ok "$1"; else bad "$1: $(printf '%q' "$2") never appeared"; fi
    else
        if raw_has "$2"; then bad "$1: $(printf '%q' "$2") appeared"; else ok "$1"; fi
    fi
}

server_gone() { ! tm has-session -t s >/dev/null 2>&1; }
alive() { tm has-session -t s >/dev/null 2>&1; }

# ---------------------------------------------------------------- polling

# _poll_until <tries> <predicate…> — 100 ms between tries.
_poll_until() {
    local tries="$1"
    shift
    local i=0
    while [ "$i" -lt "$tries" ]; do
        if "$@"; then return 0; fi
        sleep 0.1
        i=$((i + 1))
    done
    return 1
}

_vis_has() { cap | grep -qF -- "$1"; }
_all_has() { capall | grep -qF -- "$1"; }
_vis_lacks() { ! cap | grep -qF -- "$1"; }

# Pattern appears in the visible pane.
wait_vis() { _poll_until 120 _vis_has "$1"; }
# Pattern appears anywhere, scrollback included.
wait_all() { _poll_until 200 _all_has "$1"; }
# Pattern has left the visible pane.
wait_gone() { _poll_until 120 _vis_lacks "$1"; }

# wait_all_more <fixed string> <count> — the history holds MORE than <count> copies.
#
# A scenario's SECOND turn over the same document cannot `wait_all` for the same marker:
# the first turn left it in the scrollback, so that wait is over before the turn has even
# started, and the assertions that follow read the first turn's rows back (CI 34870469581,
# scenario 16's control run). Count the copies before the turn, then wait for one more.
_all_count_gt() { [ "$(count_all "$1")" -gt "$2" ]; }
wait_all_more() { _poll_until 200 _all_count_gt "$1" "$2"; }

# Three consecutive identical visible captures = the frame has stopped moving. The spinner
# animates while the app is busy (one frame per `SPINNER_TICK`, 120 ms), so a successful
# settle also proves the turn is over — PROVIDED the captures span more than one tick. Two
# captures 120 ms apart did not: the tick is "at least 120 ms", and on a runner where it lands
# a few ms late both captures see the same frame and a turn in progress is called settled
# (CI 34870469581, the macos leg). Three captures span 240 ms plus two round-trips, so a
# spinner that holds still across them is one that has stopped, not one that is late.
settle() {
    local prev="__none__" cur same=0 i=0
    while [ "$i" -lt 100 ]; do
        cur="$(cap)"
        if [ "$cur" = "$prev" ]; then
            same=$((same + 1))
            [ "$same" -ge 2 ] && return 0
        else
            same=0
        fi
        prev="$cur"
        sleep 0.12
        i=$((i + 1))
    done
    return 1
}

# ---------------------------------------------------------------- measurement

# Normalises the two live cells (spinner frame, elapsed clock) so a shape can be compared.
#
# The spinner frames are an ALTERNATION, never a bracket expression: a class of multi-byte
# characters means what it says only in a UTF-8 locale, and in the C locale `sed` reads it as
# a set of single BYTES — every `\xe2` in the row is then a match, so `⠋` became `***` and the
# `⎿` beside it lost its own lead byte (CI 34777246950, the macos leg: `LC_ALL=C.UTF-8` is
# not a locale macOS has, so its tools silently fall back to C). Spelled this way the helper
# compares the same bytes in any locale.
norm() {
    sed -E \
        -e 's/⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏/*/g' \
        -e 's/<1s/Ts/g' \
        -e 's/[0-9]+m[0-9]+s/Ts/g' \
        -e 's/[0-9]+\.[0-9]s/Ts/g' \
        -e 's/[0-9]+s/Ts/g' \
        -e 's/[0-9][0-9.]*[km]? tokens/N tokens/g'
}

# 1-based row of the first line matching a fixed string in the visible pane ("" if none).
row_of() { cap | grep -nF -- "$1" | head -1 | cut -d: -f1; }

# Occurrences of a fixed string across the visible pane / the whole history.
count_vis() { cap | grep -cF -- "$1" | tr -d ' '; }
count_all() { capall | grep -cF -- "$1" | tr -d ' '; }

# --- the frame window --------------------------------------------------------------
#
# Every frame assertion works inside the LAST pair of separator rows, never on the whole
# pane: a committed user block opens with the same `❯` glyph the composer uses, and a
# resize can strand a stale separator of the old width above the live frame. The pair is
# the only unambiguous anchor.

frame_top() { cap | grep -n '^┄┄┄' | tail -2 | head -1 | cut -d: -f1; }
frame_bot() { cap | grep -n '^┄┄┄' | tail -1 | cut -d: -f1; }

# Everything between the separators: the composer rows plus any completion-candidates row.
composer_block() {
    local t b
    t="$(frame_top)"
    b="$(frame_bot)"
    if [ -n "$t" ] && [ -n "$b" ] && [ "$b" -gt "$t" ]; then cap | sed -n "$((t + 1)),$((b - 1))p"; fi
}

# 1-based pane row of the composer's first (prompt) row.
composer_row() {
    local t off
    t="$(frame_top)"
    off="$(composer_block | grep -n '❯' | head -1 | cut -d: -f1)"
    if [ -n "$t" ] && [ -n "$off" ]; then echo $((t + off)); fi
}

count_composer() { composer_block | grep -cF -- "$1" | tr -d ' '; }

# The frame's bottom zone: the row directly under the LOWER separator (status line, the
# selected suggestion's description, or a surface's first row).
bottom_zone() {
    local bot
    bot="$(frame_bot)"
    [ -n "$bot" ] && cap | sed -n "$((bot + 1))p"
}

# The status row's MODEL segment alone — everything before the first " · " joiner.
#
# With token accounting (WP53) the row grows the context/token segments beside the model
# for a usage-reporting provider, so an assertion that is about the model surviving a resize, a
# chunked insert or a /model swap must not also be an assertion about the context figure.
# The token half has its own pin in 01-startup.sh.
status_model() { bottom_zone | sed 's/ · .*$//'; }

# Display columns of a separator row (grep -o counts runes, not bytes).
row_width() { cap | sed -n "${1}p" | grep -o '┄' | wc -l | tr -d ' '; }
sep_width() { row_width "$(frame_bot)"; }

# Distinct matches of an extended pattern across the whole history.
uniq_all() { capall | grep -oE -- "$1" | sort -u | wc -l | tr -d ' '; }
# Matches of an extended pattern that occur more than once (empty = contiguous history).
dupes_all() { capall | grep -oE -- "$1" | sort | uniq -c | awk '$1 > 1 { print $2 }'; }

# The real terminal cursor, as tmux sees it.
cursor_xy() { tm display-message -pt s '#{cursor_x} #{cursor_y}'; }

# The pane's title, as tmux keeps it (OSC 0/2 set it; CSI 22/23 t push and pop it).
pane_title() { tm display-message -pt s '#{pane_title}'; }
pane_title_is() { [ "$(pane_title)" = "$1" ]; }

# raw_offset <fixed string> — byte offset of the FIRST occurrence in the raw capture ("" if none):
# how a scenario proves one control sequence left the process before another.
raw_offset() { LC_ALL=C grep -aboF -- "$1" "$RAW" 2>/dev/null | head -1 | cut -d: -f1; }

# emu_width <text> — the display columns <text> occupies as THIS tmux counts them.
#
# `capture-pane` hands back glyphs, not cells, so a row that the app padded to its ruler's idea
# of a width cannot be measured off the capture. The emulator's own opinion is the cursor: the
# text is typed into a scratch session running `cat`, the tty echoes it, tmux renders the echo
# and `#{cursor_x}` says where its ruler left the cursor. Enter then hands the line to `cat` and
# parks the cursor back at column 0 for the next measurement. The scratch pane is 400 columns
# wide so nothing wraps, and lives on the scenario's own private server.
emu_width() {
    if ! tm has-session -t meas >/dev/null 2>&1; then
        tm new-session -d -s meas -x 400 -y 4 cat
        _poll_until 30 tm has-session -t meas
    fi
    _poll_until 30 _meas_at 0
    tm send-keys -t meas -l "$1"
    local prev="" cur="" i=0
    while [ "$i" -lt 30 ]; do
        cur="$(tm display-message -pt meas '#{cursor_x}')"
        if [ "$cur" = "$prev" ] && [ "$cur" != "0" ]; then break; fi
        prev="$cur"
        sleep 0.05
        i=$((i + 1))
    done
    tm send-keys -t meas Enter
    echo "$cur"
}
_meas_at() { [ "$(tm display-message -pt meas '#{cursor_x}')" = "$1" ]; }

# Lines that have scrolled off the top into native scrollback — tmux's own counter, and
# therefore the exact number of rows the app has inserted. Stable where `capture-pane |
# wc -l` is not (tmux trims trailing blank rows).
hist_size() { tm display-message -pt s '#{history_size}'; }

# ---------------------------------------------------------------- reporting

ok() {
    echo "PASS: $1"
    PASS=$((PASS + 1))
}
bad() {
    echo "FAIL: $1"
    FAIL=$((FAIL + 1))
}
wart() {
    echo "WART: $1"
    WARTS=$((WARTS + 1))
}

# check <description> <actual> <expected>
check() {
    if [ "$2" = "$3" ]; then ok "$1 ($2)"; else bad "$1: got '$2', want '$3'"; fi
}

# check_once <description> <fixed string> — present in history exactly once.
check_once() { check "$1" "$(count_all "$2")" 1; }

# The standard frame invariant: a separator pair at the terminal's width, exactly one
# composer row between them, and an occupied bottom zone.
check_frame_intact() {
    local label="$1" width="$2" t b
    t="$(frame_top)"
    b="$(frame_bot)"
    if [ -z "$t" ] || [ -z "$b" ] || [ "$b" -le "$t" ]; then
        bad "$label: the frame's separator pair is missing (top=$t bottom=$b)"
        return
    fi
    check "$label: top separator spans the terminal" "$(row_width "$t")" "$width"
    check "$label: bottom separator spans the terminal" "$(row_width "$b")" "$width"
    check "$label: exactly one composer row between them" "$(count_composer '❯')" 1
    if [ -n "$(bottom_zone)" ]; then
        ok "$label: the bottom zone is occupied (row $((b + 1)))"
    else
        bad "$label: the bottom zone is empty"
    fi
}

finish() {
    echo
    echo "---- $(basename "$0"): PASS=$PASS FAIL=$FAIL WARTS=$WARTS"
    [ "$FAIL" -eq 0 ] || exit 1
    exit 0
}

# ---------------------------------------------------------------- session fixtures

# write_session <id> <rounds> <tail_body_lines> — synthesises a session bundle in
# $SCEN_HOME/.iota/sessions/<id> (chat/session.go bundle layout: meta.json +
# messages.jsonl). The LAST three rounds — the resume echo window, `RESUME_ECHO_ROUNDS` —
# carry `tail_body_lines` body lines each so the echo overflows the screen.
write_session() {
    local id="$1" rounds="$2" tail_lines="$3"
    local dir="$SCEN_HOME/.iota/sessions/$id"
    mkdir -p "$dir"
    local now
    now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    : >"$dir/messages.jsonl"
    local n=0
    while [ "$n" -lt "$rounds" ]; do
        local tag
        tag="$(printf 'r%02d' "$n")"
        printf '{"role":"user","content":"%s question"}\n' "$tag" >>"$dir/messages.jsonl"
        if [ "$n" -ge $((rounds - 3)) ]; then
            local body="" i=0
            while [ "$i" -lt "$tail_lines" ]; do
                body="$body$tag body line $(printf '%02d' "$i")\\n\\n"
                i=$((i + 1))
            done
            printf '{"role":"assistant","content":"%s"}\n' "$body" >>"$dir/messages.jsonl"
        else
            printf '{"role":"assistant","content":"%s answer"}\n' "$tag" >>"$dir/messages.jsonl"
        fi
        n=$((n + 1))
    done
    cat >"$dir/meta.json" <<EOF
{
  "v": 1,
  "id": "$id",
  "created_at": "$now",
  "updated_at": "$now",
  "provider": "openai",
  "model": "fake",
  "context_window": 128000,
  "base_url": "http://127.0.0.1:$IOTA_PORT",
  "cwd": "$SCEN_TMP",
  "title": "resume fixture",
  "message_count": $((rounds * 2))
}
EOF
}
