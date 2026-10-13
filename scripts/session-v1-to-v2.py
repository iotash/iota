#!/usr/bin/env python3
"""Converts v1 session bundles to v2, once.

A v1 `meta.json` (Go's, and every iota build's before 2026-10-11) records the provider TYPE in `provider`
and the URL it actually ran against in `base_url` (none for the type's default endpoint); v2 records the
provider NAME — a `providers:` entry of the config — and no URL. iota no longer reads v1
(`SessionError::OldFormat` names this script). This rewrites each v1 meta:

  v         1 -> 2
  provider  <type> -> the provider it ran on, by the CURRENT config (below)
  base_url  removed (v2 has no such key)

and nothing else: `messages.jsonl` and the attachments are never touched, every other meta key is kept as
it is, in its place.

Which provider, in this order:

  1. `--map TYPE=NAME`: the user's decision for every bundle of TYPE — NAME is an entry of that type, or
     the type itself for its built-in endpoint;
  2. a recorded `base_url`: the ONE entry of that type whose `url:` is that URL (a trailing slash and the
     case of the scheme and host aside); two such entries, or none, are reported, not guessed;
  3. no `base_url`: the session ran on the type's default endpoint, so it is the built-in provider of
     that name — unless an entry of another type has taken the name, which is reported.

The type alone never decides: two sessions of one type can have run on two endpoints, and the URL is what
says which. The config is read by iota itself (`iota list providers`, run in this directory, so the
project's `.iota.yaml` counts as it would for a run; `--config` passes `-c`).

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
from urllib.parse import urlsplit

VERSION = 2


def providers_of(iota, config):
    """`{name: (type, url)}` for every `providers:` entry the current config defines, as iota reads it; the
    url is `""` for an entry that keeps its type's default."""
    cmd = [iota] + (["-c", config] if config else []) + ["list", "providers"]
    run = subprocess.run(cmd, capture_output=True, text=True)
    if run.returncode != 0:
        sys.exit("cannot read the config: %s failed:\n%s" % (" ".join(cmd), run.stderr.strip()))
    providers = {}
    for line in run.stdout.splitlines():
        # `  name (type: t, url: u)  [key: …]` — the type is the name itself when the line names none.
        m = re.match(r"^  (\S+)(?: \((.*)\))?  \[", line)
        if not m:
            continue
        info = m.group(2) or ""
        kind = re.search(r"(?:^|, )type: ([^,]+)", info)
        url = re.search(r"(?:^|, )url: (.+)$", info)
        providers[m.group(1)] = (kind.group(1) if kind else m.group(1), url.group(1) if url else "")
    return providers


def same_url(a, b):
    """Whether two URLs name the same endpoint: a trailing slash and the case of the scheme and host aside."""
    def norm(u):
        p = urlsplit(u.strip())
        return (p.scheme.lower(), p.netloc.lower(), p.path.rstrip("/"), p.query)
    return norm(a) == norm(b)


def resolve(kind, base_url, providers, chosen):
    """The provider a v1 bundle of type `kind` that ran against `base_url` runs on: `(name, basis, None)`,
    or `(None, None, why)` when the config does not say which. `chosen` is `--map`, checked against the
    config."""
    if kind in chosen:
        name = chosen[kind]
        if name in providers and providers[name][0] != kind:
            return None, None, "--map %s=%s: %s is of type %s" % (kind, name, name, providers[name][0])
        if name in providers or name == kind:
            return name, "--map", None
        return None, None, "--map %s=%s: the config defines no provider %s" % (kind, name, name)
    if base_url:
        names = sorted(n for n, (t, u) in providers.items() if t == kind and u and same_url(u, base_url))
        if len(names) == 1:
            return names[0], "its base_url is %s's url" % names[0], None
        if names:
            return None, None, "base_url %s is the url of %d %s providers (%s); choose with --map %s=<name>" % (
                base_url, len(names), kind, ", ".join(names), kind)
        others = sorted(n for n, (t, _) in providers.items() if t == kind)
        return None, None, "base_url %s is the url of no %s provider in the config (the %s providers: %s); add one, or choose with --map %s=<name>" % (
            base_url, kind, kind, ", ".join(others) or "none", kind)
    if kind in providers and providers[kind][0] != kind:
        return None, None, ("no base_url: it ran on the built-in %s endpoint, but the config's provider %s is "
                            "of type %s; add a provider of type %s without a url and --map %s=<it>" % (
                                kind, kind, providers[kind][0], kind, kind))
    return kind, "no base_url: v1 records the URL it ran against, so it ran on the built-in %s endpoint" % kind, None


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
        name, basis, why = resolve(kind, meta.get("base_url") or "", providers, chosen)
        if name is None:
            print("%s: SKIPPED — %s (%s)" % (d.name, why, d))
            left += 1
            continue
        removed = "; removed base_url %s" % json.dumps(meta["base_url"]) if "base_url" in meta else ""
        print("%s: v %s -> %d; provider %s -> %s [%s]%s (%s)" % (
            d.name, version, VERSION, json.dumps(kind), json.dumps(name), basis, removed, d))
        if args.write:
            write(path, convert(meta, name))
        changed += 1

    verb = "converted" if args.write else "to convert (dry run: rerun with --write)"
    print("%d %s, %d left alone, %d already v%d" % (changed, verb, left, current, VERSION))
    return 1 if left else 0


if __name__ == "__main__":
    sys.exit(main())
