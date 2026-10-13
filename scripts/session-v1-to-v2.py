#!/usr/bin/env python3
"""Converts v1 session bundles to v2, once.

A v1 `meta.json` (Go's, and every iota build's before 2026-10-11) records the provider TYPE in `provider`
and the endpoint's `base_url`; v2 records the provider NAME — a `providers:` entry of the config — and no
URL. iota no longer reads v1 (`SessionError::OldFormat` names this script). This rewrites each v1 meta:

  v         1 -> 2
  provider  <type> -> the one `providers:` entry of that type in the CURRENT config
  base_url  removed (v2 has no such key)

and nothing else: `messages.jsonl` and the attachments are never touched, every other meta key is kept as
it is, in its place.

The config is read by iota itself (`iota list providers`, run in this directory, so the project's
`.iota.yaml` counts as it would for a run; `--config` passes `-c`). A type that no entry has, or that
several entries have, is NOT guessed: the bundle is reported and left alone, and `--map TYPE=NAME`
decides — NAME is an entry of the config, or the type itself for its built-in endpoint.

A dry run by default: it reports what it would change and writes nothing. `--write` applies it. A bundle
already v2 is skipped, so running it again changes nothing. Exit status 1 when a bundle was left alone.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

VERSION = 2


def providers_of(iota, config):
    """`{name: type}` for every `providers:` entry the current config defines, as iota reads it."""
    cmd = [iota] + (["-c", config] if config else []) + ["list", "providers"]
    run = subprocess.run(cmd, capture_output=True, text=True)
    if run.returncode != 0:
        sys.exit("cannot read the config: %s failed:\n%s" % (" ".join(cmd), run.stderr.strip()))
    providers = {}
    for line in run.stdout.splitlines():
        # `  name (type: t, url: u)  [key: …]` — the type is the name itself when the line names none.
        m = re.match(r"^  (\S+)(?: \(([^)]*)\))?  \[", line)
        if not m:
            continue
        kind = re.search(r"(?:^|, )type: ([^,]+)", m.group(2) or "")
        providers[m.group(1)] = kind.group(1) if kind else m.group(1)
    return providers


def resolve(kind, providers, chosen):
    """The provider name a v1 bundle of type `kind` runs on: `(name, None)`, or `(None, why)` when the config
    does not say which. `chosen` is `--map`: the user's own decision, checked against the config."""
    if kind in chosen:
        name = chosen[kind]
        if name in providers and providers[name] != kind:
            return None, "--map %s=%s: %s is of type %s" % (kind, name, name, providers[name])
        if name in providers or name == kind:
            return name, None
        return None, "--map %s=%s: the config defines no provider %s" % (kind, name, name)
    names = sorted(n for n, t in providers.items() if t == kind)
    if len(names) == 1:
        return names[0], None
    if names:
        return None, "type %s has %d providers in the config (%s); choose one with --map %s=<name>" % (
            kind, len(names), ", ".join(names), kind)
    return None, ("no provider in the config has type %s; add one, or run it on the built-in endpoint "
                  "with --map %s=%s" % (kind, kind, kind))


def bundles(root):
    """Every bundle directory under the sessions root: the flat ones and `projects/<bucket>/<id>`."""
    if not root.is_dir():
        return []
    flat = [d for d in root.iterdir() if d.name != "projects"]
    projects = root / "projects"
    bucketed = [d for b in projects.iterdir() if b.is_dir() for d in b.iterdir()] if projects.is_dir() else []
    return sorted(d for d in flat + bucketed if (d / "meta.json").is_file())


def convert(meta, name):
    """The v2 meta: `v` and `provider` rewritten in place, `base_url` dropped, every other key as it was."""
    out = {"v": VERSION} if "v" not in meta else {}
    for key, value in meta.items():
        if key == "base_url":
            continue
        out[key] = VERSION if key == "v" else name if key == "provider" else value
    return out


def write(path, meta):
    """`meta.json` as iota writes it: 2-space indent, no trailing newline, through a temp file and a rename."""
    tmp = path.with_name("meta.json.tmp")
    tmp.write_text(json.dumps(meta, indent=2, ensure_ascii=False), encoding="utf-8")
    os.replace(tmp, path)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--write", action="store_true", help="apply the changes (default: report only)")
    p.add_argument("--map", action="append", default=[], metavar="TYPE=NAME",
                   help="run the v1 bundles of TYPE on provider NAME (repeatable)")
    p.add_argument("--iota", default="iota", help="the iota binary that reads the config (default: iota)")
    p.add_argument("--config", help="the config file, as iota's -c")
    p.add_argument("--sessions", type=Path, default=Path.home() / ".iota" / "sessions",
                   help="the sessions root (default: ~/.iota/sessions)")
    args = p.parse_args(argv)
    chosen = {}
    for m in args.map:
        kind, sep, name = m.partition("=")
        if not sep or not kind or not name:
            p.error("--map wants TYPE=NAME, got %r" % m)
        chosen[kind] = name

    providers = providers_of(args.iota, args.config)
    changed = left = current = 0
    for d in bundles(args.sessions):
        path = d / "meta.json"
        try:
            meta = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as e:
            print("%s: SKIPPED — meta.json cannot be read: %s" % (d.name, e))
            left += 1
            continue
        version = meta.get("v", 0)
        if not isinstance(version, int) or version >= VERSION:
            current += 1
            continue
        kind = meta.get("provider", "")
        name, why = resolve(kind, providers, chosen)
        if name is None:
            print("%s: SKIPPED — %s (%s)" % (d.name, why, d))
            left += 1
            continue
        removed = "; removed base_url %s" % json.dumps(meta["base_url"]) if "base_url" in meta else ""
        print("%s: v %s -> %d; provider %s -> %s%s (%s)" % (
            d.name, version, VERSION, json.dumps(kind), json.dumps(name), removed, d))
        if args.write:
            write(path, convert(meta, name))
        changed += 1

    verb = "converted" if args.write else "to convert (dry run: rerun with --write)"
    print("%d %s, %d left alone, %d already v%d" % (changed, verb, left, current, VERSION))
    return 1 if left else 0


if __name__ == "__main__":
    sys.exit(main())
