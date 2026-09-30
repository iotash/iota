#!/usr/bin/env bash
# Bot retention experiment (docs/design/bot-mode.md §5.2, the L1 acceptance item): how much of what a bot
# was told survives N compactions. Its numbers are what FINALISE §6 #13 — the v1 caps (MEMORY.md 8 KiB with
# a 6 KiB soft threshold, 500 B per line, a 1500-word summary) are provisional until this has been run.
#
# OPT-IN, MANUAL, NEVER IN CI: it drives a real model and spends real tokens. It is not called by ci.sh.
#
# What it does, per group: a fresh bot in a private tmux server, driven through its TUI (a bot has no
# headless form: `iota run <bot> -m` is refused). A few filler turns, then ONE message carrying 20 facts —
# ten worth keeping beyond the conversation (preferences, decisions, names: memory material) and ten that
# are conversation state (the step I am on, the test that fails right now) — then filler until the log holds
# N compactions, then one quiz asking for all 20. Each answer is graded by a keyword.
#
# The three groups compared:
#   noflush  compactions WITHOUT the memory flush: the script types `/compact` every
#            RETENTION_COMPACT_EVERY filler turns, which keeps the usage below the bot's threshold, so the
#            flush never triggers (a hand-typed /compact records `flush_skipped` and does not flush). The
#            `remember` tool is still there: what the model saves of its own accord counts, as it would.
#            A flush notice found in this group's log marks the group CONTAMINATED in the results.
#   flush    the v1 mechanism: nothing typed but the conversation; each compaction follows its flush turn.
#   recall   flush plus the L2 `recall` tool. DEPENDS ON L2, which is not in v1: the group is skipped unless
#            RETENTION_RECALL_SET names the toolset that provides `recall`, which the config then enables.
#
# Isolation: every group runs under a temporary HOME inside RETENTION_OUT, with a scratch config written
# there and a scratch working directory — the real ~/.iota (config, sessions, bots, memory) is never read or
# written. The script refuses to start if the scratch HOME would be the real one, and aborts a group whose
# bot directory does not appear under the scratch HOME. Everything is kept in RETENTION_OUT for inspection
# (logs, MEMORY.md, pane captures); no API key is written anywhere.
#
# Usage:
#   cargo build                                  # or point IOTA_BIN at a release build
#   export OPENAI_API_KEY=...                    # whatever key variable the provider type reads
#   RETENTION_MODEL=gpt-5.2 scripts/bot-retention.sh [group...]     # default: noflush flush recall
#
# Needs: bash, tmux, python3 (to read the JSONL log).
#
# Knobs (environment):
#   IOTA_BIN                 the iota binary                      (default: target/debug/iota)
#   RETENTION_TYPE           provider type                        (default: openai)
#   RETENTION_URL            provider base URL                    (default: the type's own)
#   RETENTION_MODEL          model id                             (required)
#   RETENTION_WINDOW         context window the bot runs under    (default: 16000 — small, so compactions are
#                            cheap; the bot's threshold is window minus max(32k, 25%), capped at half)
#   RETENTION_COMPACTIONS    compactions between facts and quiz   (default: 3)
#   RETENTION_LEAD           filler turns before the facts        (default: 2)
#   RETENTION_COMPACT_EVERY  noflush: filler turns per /compact   (default: 4)
#   RETENTION_MAX_TURNS      filler turns before giving up        (default: 120)
#   RETENTION_TIMEOUT        seconds one turn may take            (default: 300)
#   RETENTION_RECALL_SET     the toolset providing L2 `recall`    (default: empty — group skipped)
#   RETENTION_OUT            where everything is kept             (default: a new temp directory)
#
# Output: one row per group on stdout and in $RETENTION_OUT/results.tsv — compactions, flush notices,
# MEMORY.md bytes, facts recalled (memory kind / state kind / total). Read the three rows side by side;
# the per-fact grades are in $RETENTION_OUT/<group>/grades.tsv.
# bash 3.2 compatible.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

: "${IOTA_BIN:=$root/target/debug/iota}"
: "${RETENTION_TYPE:=openai}"
: "${RETENTION_URL:=}"
: "${RETENTION_MODEL:?RETENTION_MODEL must name the model id to run the bot on}"
: "${RETENTION_WINDOW:=16000}"
: "${RETENTION_COMPACTIONS:=3}"
: "${RETENTION_LEAD:=2}"
: "${RETENTION_COMPACT_EVERY:=4}"
: "${RETENTION_MAX_TURNS:=120}"
: "${RETENTION_TIMEOUT:=300}"
: "${RETENTION_RECALL_SET:=}"
: "${RETENTION_OUT:=$(mktemp -d "${TMPDIR:-/tmp}/iota-retention.XXXXXX")}"

BOT=retention
SPINNER='⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏' # an alternation, never a bracket expression (C locale, multi-byte)
FLUSH_MARK='The conversation is about to be compacted'

[ -x "$IOTA_BIN" ] || { echo "bot-retention: no iota binary at $IOTA_BIN (cargo build first)" >&2; exit 2; }
command -v tmux >/dev/null || { echo "bot-retention: needs tmux" >&2; exit 2; }
command -v python3 >/dev/null || { echo "bot-retention: needs python3" >&2; exit 2; }
mkdir -p "$RETENTION_OUT"
RETENTION_OUT="$(cd "$RETENTION_OUT" && pwd -P)"
real_home="$(cd "${HOME:?}" && pwd -P)"
case "$RETENTION_OUT/" in
    "$real_home/") echo "bot-retention: RETENTION_OUT is the real HOME; refusing" >&2; exit 2 ;;
esac

# ---------------------------------------------------------------- the facts

# id|kind|what the user says|the quiz question|grading pattern (grep -Ei)
FACTS='M1|memory|I always want commit messages prefixed with the ticket key QX- and written in the imperative.|What prefix do my commit messages carry?|QX-
M2|memory|My staging server is called bramblewood.|What is my staging server called?|bramblewood
M3|memory|We decided on SQLite rather than Postgres for the cache, because the cache must work without a network.|Which database did we choose for the cache?|sqlite
M4|memory|My cat is called Orzo.|What is my cat called?|orzo
M5|memory|I never review code after 2 pm; mornings only.|After what time do I stop reviewing code?|2 ?pm|14[:.]?00
M6|memory|The release codename this year is Periwinkle.|What is this year'"'"'s release codename?|periwinkle
M7|memory|I prefer answers in British English.|Which variety of English do I prefer?|british
M8|memory|Our API rate limit is 4417 requests per hour.|What is our API rate limit per hour?|4417
M9|memory|We never deploy on Fridays; that is a team rule.|On which weekday do we never deploy?|friday
M10|memory|My manager is Tamsin Okafor.|What is my manager'"'"'s surname?|okafor
S1|state|Right now I am on step 3 of the 7-step migration checklist.|Which step of the migration checklist was I on?|(^|[^0-9])3([^0-9]|$)|three
S2|state|The test failing at the moment is test_ledger_rollover.|Which test was failing?|ledger_rollover
S3|state|I just renamed my branch to fix/overflow-guard.|What did I rename my branch to?|overflow-guard
S4|state|The current build number is 20931.|What was the build number?|20931
S5|state|I have src/quartz.rs open in my editor.|Which file did I have open?|quartz
S6|state|The last benchmark run took 812 ms.|How long did the last benchmark run take?|812
S7|state|I am waiting on the vendor about ticket 55-A.|Which vendor ticket am I waiting on?|55-?A
S8|state|The temporary workaround is setting RETRY_JITTER=0.|What was the temporary workaround?|RETRY_JITTER
S9|state|Today'"'"'s standup moved to room Kestrel.|Which room did the standup move to?|kestrel
S10|state|The draft PR I am working on is number 1288.|What is the number of my draft PR?|1288'

TOPICS='the history of lighthouses
how sourdough starters work
the water cycle
the design of mechanical watches
how bees communicate
the origins of chess
tides and the moon
how suspension bridges carry load
the life cycle of stars
paper making by hand
how glaciers shape valleys
the invention of the printing press'

facts_message() {
    echo "Some context from my side before we carry on. Nothing to do about it now; acknowledge in one line."
    echo
    echo "$FACTS" | awk -F'|' '{ print "- " $3 }'
}

quiz_message() {
    echo "A quiz on what you know about me and my work. Answer from what you actually know: if you do not know, write unknown — do not guess. One line per question, formatted N: answer."
    echo
    echo "$FACTS" | awk -F'|' '{ print NR ". " $4 }'
}

filler_message() {
    local n=$1 topic
    topic="$(echo "$TOPICS" | sed -n "$(( (n - 1) % 12 + 1 ))p")"
    echo "Filler $n: write about 250 words on $topic. It is unrelated to anything earlier; do not refer to earlier messages."
}

# ---------------------------------------------------------------- the log

# log_query <messages.jsonl> <finals|markers|flushes|last> — read with python3 (JSON in bash is a trap).
log_query() {
    python3 - "$1" "$2" "$FLUSH_MARK" <<'PY'
import json, sys
path, what, flush_mark = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    recs = [json.loads(l) for l in open(path, encoding="utf-8") if l.strip()]
except FileNotFoundError:
    recs = []
finals = [r for r in recs if r.get("role") == "assistant" and not r.get("tool_calls")]
if what == "finals":
    print(len(finals))
elif what == "markers":
    print(sum(1 for r in recs if r.get("role") == "compaction"))
elif what == "flushes":
    print(sum(1 for r in recs if r.get("role") == "user" and r.get("content", "").startswith(flush_mark)))
elif what == "last":
    print(finals[-1].get("content", "") if finals else "")
PY
}

# ---------------------------------------------------------------- one group

run_group() {
    local group=$1 tools=''
    case "$group" in
        noflush | flush) ;;
        recall)
            if [ -z "$RETENTION_RECALL_SET" ]; then
                echo "bot-retention: group recall skipped — it needs L2 recall; set RETENTION_RECALL_SET once it exists" >&2
                printf '%s\tskipped (needs L2 recall)\n' "$group" >>"$RETENTION_OUT/results.tsv"
                return 0
            fi
            tools="    tools: { $RETENTION_RECALL_SET: }"
            ;;
        *) echo "bot-retention: unknown group $group (noflush, flush, recall)" >&2; return 2 ;;
    esac

    local dir="$RETENTION_OUT/$group"
    local home="$dir/home" work="$dir/work"
    local sock="iota-retention-$$-$group"
    mkdir -p "$home" "$work"
    local url_line=''
    [ -n "$RETENTION_URL" ] && url_line="    url: \"$RETENTION_URL\""
    {
        echo 'providers:'
        echo '  p:'
        echo "    type: $RETENTION_TYPE"
        if [ -n "$url_line" ]; then echo "$url_line"; fi
        echo 'models:'
        echo '  m:'
        echo '    provider: p'
        echo "    id: $RETENTION_MODEL"
        echo "    context_window: $RETENTION_WINDOW"
        echo 'agents:'
        echo "  $BOT:"
        echo '    mode: bot'
        echo '    model: m'
        echo '    system: "You are a helpful assistant."'
        if [ -n "$tools" ]; then echo "$tools"; fi
    } >"$home/.iota.yaml"

    tm() { tmux -L "$sock" "$@"; }
    # The visible grid (what a spinner lives in), and the grid with its scrollback (for the record).
    cap() { tm capture-pane -pt s 2>/dev/null || true; }
    capall() { tm capture-pane -pt s -S -2000 2>/dev/null || true; }
    # A turn is over when the log has one more final reply than before and the pane has stopped: no
    # spinner and the same log size over three looks a second apart (a flush turn and its compaction
    # follow a turn at once, so this waits for them too).
    log() { find "$home/.iota/sessions" -name messages.jsonl 2>/dev/null | head -1; }
    size() { local l; l="$(log)"; [ -n "$l" ] && wc -c <"$l" || echo 0; }
    wait_idle() {
        local calm=0 last='' now
        while [ "$calm" -lt 3 ]; do
            sleep 1
            now="$(size)"
            if cap | grep -Eq "$SPINNER" || [ "$now" != "$last" ]; then calm=0; else calm=$((calm + 1)); fi
            last="$now"
        done
    }
    # wait_count <finals|markers> <at least> — then idle; fails after RETENTION_TIMEOUT.
    wait_count() {
        local waited=0
        until [ "$(log_query "$(log)" "$1")" -ge "$2" ]; do
            sleep 1
            waited=$((waited + 1))
            if [ "$waited" -ge "$RETENTION_TIMEOUT" ]; then
                echo "bot-retention: $group: no $1 >= $2 after ${RETENTION_TIMEOUT}s; the pane:" >&2
                cap | tail -25 >&2
                return 1
            fi
        done
        wait_idle
    }
    # One line is typed; more than one is a bracketed paste, so its newlines do not submit it.
    send() {
        case "$1" in
            *"
"*)
                local f="$dir/.send"
                printf '%s' "$1" >"$f"
                tm load-buffer -b msg "$f"
                tm paste-buffer -p -d -b msg -t s
                ;;
            *) tm send-keys -t s -l "$1" ;;
        esac
        sleep 0.5
        tm send-keys -t s Enter
    }
    turn() {
        local before
        before="$(log_query "$(log)" finals)"
        send "$1"
        wait_count finals $((before + 1))
    }
    # A group that cannot go on stops here; what it got so far stays in $dir.
    give_up() {
        capall >"$dir/pane.txt"
        tm kill-server >/dev/null 2>&1 || true
        return 1
    }

    tm kill-server >/dev/null 2>&1 || true
    tm new-session -d -s s -x 120 -y 40 -c "$work" \
        "env -u HERDR_ENV HOME='$home' '$IOTA_BIN' run $BOT; sleep 600"
    local waited=0
    until cap | grep -q '❯'; do
        sleep 1
        waited=$((waited + 1))
        [ "$waited" -lt 60 ] || { echo "bot-retention: $group: the TUI never came up" >&2; cap | tail -25 >&2; tm kill-server; return 1; }
    done
    [ -d "$home/.iota/bots/$BOT" ] || {
        echo "bot-retention: $group: no bot directory under the scratch HOME; aborting" >&2
        tm kill-server
        return 1
    }

    local n=0
    while [ "$n" -lt "$RETENTION_LEAD" ]; do
        n=$((n + 1))
        turn "$(filler_message $n)" || { give_up; return 1; }
    done
    turn "$(facts_message)" || { give_up; return 1; }
    local base_markers
    base_markers="$(log_query "$(log)" markers)"
    local filled=0
    while [ "$(log_query "$(log)" markers)" -lt $((base_markers + RETENTION_COMPACTIONS)) ]; do
        [ "$filled" -lt "$RETENTION_MAX_TURNS" ] || {
            echo "bot-retention: $group: $RETENTION_MAX_TURNS filler turns and still short of $RETENTION_COMPACTIONS compactions" >&2
            break
        }
        n=$((n + 1)); filled=$((filled + 1))
        turn "$(filler_message $n)" || { give_up; return 1; }
        if [ "$group" = noflush ] && [ $((filled % RETENTION_COMPACT_EVERY)) -eq 0 ]; then
            local m
            m="$(log_query "$(log)" markers)"
            send "/compact"
            wait_count markers $((m + 1)) || { give_up; return 1; }
        fi
    done
    turn "$(quiz_message)" || { give_up; return 1; }
    capall >"$dir/pane.txt"
    tm kill-server >/dev/null 2>&1 || true

    # ---- grade
    local answers="$dir/answers.txt"
    log_query "$(log)" last >"$answers"
    cp "$(log)" "$dir/messages.jsonl"
    local mem="$home/.iota/bots/$BOT/MEMORY.md"
    if [ -f "$mem" ]; then cp "$mem" "$dir/MEMORY.md"; fi
    local mem_ok=0 state_ok=0 k=0 id kind _say _ask pat line grade
    : >"$dir/grades.tsv"
    while IFS='|' read -r id kind _say _ask pat; do
        k=$((k + 1))
        line="$(grep -E "^[[:space:]*]*$k[:.)]" "$answers" | head -1 || true)"
        line="${line#*[:.)]}"
        grade=miss
        if [ -n "$line" ] && ! echo "$line" | grep -Eiq '^[[:space:]*]*unknown' && echo "$line" | grep -Eiq "$pat"; then
            grade=ok
            if [ "$kind" = memory ]; then mem_ok=$((mem_ok + 1)); else state_ok=$((state_ok + 1)); fi
        fi
        printf '%s\t%s\t%s\t%s\n' "$id" "$kind" "$grade" "$line" >>"$dir/grades.tsv"
    done <<EOF
$FACTS
EOF
    local markers flushes bytes note=''
    markers="$(log_query "$dir/messages.jsonl" markers)"
    flushes="$(log_query "$dir/messages.jsonl" flushes)"
    bytes=0
    if [ -f "$dir/MEMORY.md" ]; then bytes="$(wc -c <"$dir/MEMORY.md" | tr -d ' ')"; fi
    if [ "$group" = noflush ] && [ "$flushes" -gt 0 ]; then note='CONTAMINATED: a flush ran'; fi
    printf '%s\t%s\t%s\t%s\t%s/10\t%s/10\t%s/20\t%s\n' "$group" "$markers" "$flushes" "$bytes" \
        "$mem_ok" "$state_ok" $((mem_ok + state_ok)) "$note" >>"$RETENTION_OUT/results.tsv"
}

groups="${*:-noflush flush recall}"
printf 'group\tcompactions\tflush notices\tMEMORY.md bytes\tmemory kind\tstate kind\ttotal\tnote\n' >"$RETENTION_OUT/results.tsv"
for g in $groups; do
    run_group "$g" || echo "bot-retention: group $g failed; see $RETENTION_OUT/$g" >&2
done
column -t -s "$(printf '\t')" "$RETENTION_OUT/results.tsv" 2>/dev/null || cat "$RETENTION_OUT/results.tsv"
echo "bot-retention: everything is in $RETENTION_OUT"
