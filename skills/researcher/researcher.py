#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# The Researcher. Copyright (c) 2026 LudaLunizz
# Licensed under the MIT License; see the file LICENSE for details.
"""The Researcher: a research loader for Claude (saved research into a Claude Code session).

Usage: researcher.py [SET ...]             popup, then write the ticked docs as .txt files
       researcher.py --setup SET [...]     popup only, to change the saved defaults
       researcher.py --no-popup SET [...]  use the saved defaults, ask nothing
       researcher.py --list                show sets, saved defaults, library size
       researcher.py --sync                make/refresh the plain-text copy of the whole library
       researcher.py --usage               local totals: characters and tokens saved
       researcher.py --usage-export [FILE] write the anonymous usage records to a file (not sent)
       researcher.py --usage-share on|off  share the anonymous usage counts with the project or not
       researcher.py --usage-send [--dry-run]  send the records not sent yet (only when sharing is on)
       researcher.py --init [LIBRARY]      write a starter config.toml
       researcher.py --sessions | --rename ID NAME | --resume ID | --session-load ID
                                           closed-session tracking (opt-in: [sessions] in config)

A "library" is a folder of documents (.html, .md, .txt, .pdf). A "set" is a named list
of documents from it, defined in config.toml. A GTK 4 popup lets the user tick what to
load, so no tokens are spent on documents that are not needed. The ticked documents are
read from a plain-text copy in the researcher's own folder (made once per document, redone
when the source changes; PDF and HTML are squeezed for tokens) and their paths are printed,
so the skill can Read them in full. Nothing in the library is ever changed or moved.

Sets:
- defined sets: `[sets.NAME]` in config.toml with `docs` (ids or globs like "rust/*")
  and `include` (other sets);
- `all`: every document in the library;
- custom sets: any other name made of lowercase letters, digits, `.`, `_`, `-`. The first
  call lists the whole library with nothing ticked; what the user ticks is saved as that
  set. Later calls show the chosen docs ticked, the rest folded away, and documents added
  since marked NEW (never ticked on their own).

A document id is its path inside the library without the extension (`rust/memory`). A
folder that holds an `index.html` / `index.md` counts as one document named after the
folder. `exclude = [globs]` in config.toml leaves documents out of the library. When `x.html` and `x.pdf` both exist, the HTML one is used.
"""
import argparse
import atexit
import contextlib
import datetime
import fnmatch
import html
import json
import os
import re
import shlex
import stat
import subprocess
import sys
import urllib.error
import urllib.request
import uuid

try:
    import tomllib
except ImportError:  # Python < 3.11
    tomllib = None

APP = "researcher"
DOC_EXTS = (".html", ".htm", ".md", ".txt", ".pdf")  # preference order when names collide
INDEX_NAMES = tuple("index" + e for e in DOC_EXTS)
POPUP_TIMEOUT = 540  # seconds; the skill runs this with a 600 s Bash timeout
NAME_RE = re.compile(r"[a-z0-9][a-z0-9._-]*")
RESERVED = {"all"}
CHARS_PER_K = 2400  # ~2.4 chars per token, measured with count_tokens (see README)
SKIPPED_NAMED = 20  # up to this many left-out documents are named; more are only counted


class ConfigError(Exception):
    pass


# ---------------------------------------------------------------- paths and config

def config_home():
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    return os.path.join(base, APP)


def cache_home():
    base = os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache")
    return os.path.join(base, APP)


def text_home():
    """The researcher's own folder: a plain-text copy of every library document."""
    base = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
    return os.path.join(base, APP, "text")


def load_config(path):
    """config.toml as a dict; {} when the file does not exist."""
    try:
        with open(path, "rb") as f:
            raw = f.read()
    except FileNotFoundError:
        return {}
    if tomllib is None:
        raise ConfigError("Python 3.11 or newer is needed to read config.toml")
    try:
        return tomllib.loads(raw.decode("utf-8"))
    except (tomllib.TOMLDecodeError, UnicodeDecodeError) as e:
        raise ConfigError(f"{path}: {e}")


def library_path(cfg, override=None):
    p = override or os.environ.get("RESEARCHER_LIBRARY") or cfg_str(cfg, "library")
    return os.path.abspath(os.path.expanduser(p)) if p else None


def cfg_str(cfg, key):
    """A string value of the config, or ConfigError naming the key."""
    v = cfg.get(key)
    if v is not None and not isinstance(v, str):
        raise ConfigError(f"`{key}` must be a string")
    return v


def str_list(v, what):
    """A list of strings from the config (`exclude`, `docs`, `include`)."""
    if v is None:
        return []
    if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
        raise ConfigError(f"{what} must be a list of strings")
    return v


STARTER = """\
# The Researcher: config
# The folder that holds your research documents (.html, .md, .txt, .pdf).
library = "{library}"

# Where the plain-text copy of your library is kept (default ~/.local/share/researcher/text).
# Your own files are only read, never changed; Claude reads these copies instead.
# text_dir = "~/.local/share/researcher/text"

# Documents to leave out of the library entirely (id globs).
# exclude = ["scratch/*", "drafts/*"]

# Used when /researcher is run without a set name.
# default_set = "main"

# Short names for sets.
# [aliases]
# rs = "rust"

# A set is a list of document ids (path inside the library, no extension).
# Globs work: "rust/*". `include` pulls in other sets.
# [sets.rust]
# docs = ["rust/linux-notes", "rust/memory-*"]
#
# [sets.myapp]
# include = ["rust"]
# docs = ["myapp/roadmap"]

# A local, anonymous usage log (counts and character sums, never titles or paths) feeds
# `researcher.py --usage`. `share = true` (or `researcher.py --usage-share on`) also sends those
# counts to the project; nothing is sent unless you set it. Switch the log off with:
# [usage]
# log = false
# share = false
"""


def init_config(path, library):
    if os.path.exists(path):
        print(f"{path} already exists; not overwriting")
        return 1
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(STARTER.format(library=library.replace("\\", "\\\\").replace('"', '\\"')))
    except OSError as e:
        print(f"CONFIG ERROR: {path}: {e}")
        return 6
    print(f"wrote {path}")
    return 0


# ---------------------------------------------------------------- the library

def scan(library, exclude=()):
    """{doc id: file path} for every document under `library`, minus ids matching `exclude` globs."""
    found = {}  # id -> (preference, path)

    def offer(doc_id, path, ext):
        pref = DOC_EXTS.index(ext)
        if doc_id not in found or pref < found[doc_id][0]:
            found[doc_id] = (pref, path)

    for root, dirs, files in os.walk(library):
        try:
            root.encode("utf-8")  # names that are not UTF-8 are left out, as in the Rust version
        except UnicodeEncodeError:
            dirs[:] = []
            continue
        dirs[:] = sorted(d for d in dirs if not d.startswith("."))
        rel = os.path.relpath(root, library)
        rel = "" if rel == "." else rel.replace(os.sep, "/")
        index = next((n for n in INDEX_NAMES if n in files), None)
        if index and rel:  # a folder with an index file is one document
            offer(rel, os.path.join(root, index), os.path.splitext(index)[1])
            dirs[:] = []
            continue
        for fn in sorted(files):
            stem, ext = os.path.splitext(fn)
            if fn.startswith(".") or ext.lower() not in DOC_EXTS:
                continue
            path = os.path.join(root, fn)
            try:
                path.encode("utf-8")
            except UnicodeEncodeError:
                continue
            if os.path.isfile(path):
                offer(f"{rel}/{stem}" if rel else stem, path, ext.lower())
    return {k: v[1] for k, v in sorted(found.items())
            if not any(fnmatch.fnmatchcase(k, pat) for pat in exclude)}


# Comments and <script>/<style>/<svg> in one left-to-right pass, so a <!-- inside a script is
# script text. <!--> and <!---> are empty comments; an unclosed comment runs to the end.
HIDDEN_RE = re.compile(r"(?is)<!--(?:>|->|.*?(?:-->|\Z))|<script\b.*?</script>|<style\b.*?</style>|<svg\b.*?</svg>")
# Markdown: closed comments, and an unclosed one only when it starts a line (CommonMark HTML block).
MD_COMMENT_RE = re.compile(r"(?ms)<!--(?:>|->|.*?-->)|^<!--.*")


def drop_comments(raw):
    """Remove <!-- ... --> comments of a Markdown file as a unit, so nothing hidden in one
    reaches the model; a <!-- inside inline code does not swallow the rest of the file."""
    return MD_COMMENT_RE.sub(" ", raw)


def html_text(raw):
    t = HIDDEN_RE.sub(" ", raw)  # before the tag step: a '>' inside a comment must not end it early
    t = re.sub(r"(?i)<br\s*/?>|</(p|div|li|tr|h[1-6]|section|table)>", "\n", t)
    t = re.sub(r"(?i)<(td|th)\b[^>]*>", " | ", t)
    t = re.sub(r"(?i)<li\b[^>]*>", "- ", t)
    t = re.sub(r"<[A-Za-z/!?][^>]*>", " ", t)
    t = html.unescape(t)
    t = re.sub(r"[ \t\r\f\v]+", " ", t)
    t = re.sub(r"\n\s*\n+", "\n\n", t)
    return "\n".join(line.strip() for line in t.splitlines()).strip()


TEXT_VERSION = 5  # bump when to_text(), drop_comments(), free_store_name() or squeeze() changes: every stored copy is made again
INVISIBLE = dict.fromkeys(map(ord, "\u200b\u200c\u200d\u2060\ufeff\u00ad"))  # zero-width, soft hyphen
PAGE_NUMBER_RE = re.compile(r"(?i)^(page\s+)?\d{1,4}(\s*(/|of)\s*\d{1,4})?$")
PAGE_WORD_RE = re.compile(r"(?i)\b(page|pagina|blz|seite|p\.)\s*\d")
RULE_RE = re.compile(r"^[\s|_=*~.\u00b7\u2022\u2500-\u257f-]{3,}$")  # lines of only decoration


def furniture_key(line):
    """Page-numbered lines ("Report | page 3 of 9") match across pages; others only exactly."""
    k = line.strip()
    return re.sub(r"\d+", "#", k) if PAGE_WORD_RE.search(k) else k


def drop_page_furniture(pages):
    """Remove page numbers and headers/footers that repeat on most pages of a PDF."""
    cands = []  # per page: indexes of the first/last three non-empty lines
    for lines in pages:
        idx = [i for i, ln in enumerate(lines) if ln.strip()]
        cands.append(set(idx[:3] + idx[-3:]))
    seen = {}
    for lines, cand in zip(pages, cands):
        for key in {furniture_key(lines[i]) for i in cand}:
            seen[key] = seen.get(key, 0) + 1
    need = max(3, int(len(pages) * 0.4))
    out = []
    for lines, cand in zip(pages, cands):
        keep = []
        for i, ln in enumerate(lines):
            k = ln.strip()
            if i in cand and k and (PAGE_NUMBER_RE.match(k) or seen[furniture_key(k)] >= need):
                continue
            keep.append(ln)
        out.append(keep)
    return out


def squeeze(text, kind):
    """Fewer tokens, same content. kind: 'pdf', 'html' or 'plain' (md/txt are only tidied)."""
    text = text.replace("\r\n", "\n").replace("\r", "\n").replace("\u00a0", " ").translate(INVISIBLE)
    if kind == "pdf":
        pages = [pg.split("\n") for pg in text.split("\f")]
        text = "\n".join("\n".join(pg) for pg in (drop_page_furniture(pages) if len(pages) >= 3 else pages))
    out, blank = [], True
    for ln in text.split("\n"):
        ln = ln.rstrip()
        if kind != "plain":  # layout whitespace means nothing here; markdown/text may use it
            ln = re.sub(r" {3,}", " | ", ln.strip())
            ln = re.sub(r"[ \t]{2,}", " ", ln)
            if RULE_RE.match(ln):
                ln = ""
        elif re.fullmatch(r"([-_=*~])\1{4,}", ln.strip()):
            ln = ""
        if not ln.strip():
            if not blank:
                out.append("")
            blank = True
            continue
        if kind != "plain" and out and ln == out[-1]:  # an exact repeat (Markdown/text keep theirs: code repeats)
            continue
        out.append(ln)
        blank = False
    return "\n".join(out).strip()


def convert(path):
    """(text before squeezing, squeezed text) of one document. Raises OSError / RuntimeError."""
    ext = os.path.splitext(path)[1].lower()
    if ext == ".pdf":
        try:
            r = subprocess.run(["pdftotext", "-layout", path, "-"], capture_output=True, check=True)
        except FileNotFoundError:
            raise RuntimeError("pdftotext not found (install poppler-utils)")
        except subprocess.CalledProcessError as e:
            err = e.stderr.decode("utf-8", "replace").strip()
            raise RuntimeError(f"pdftotext failed: {err or e.returncode}")
        out = r.stdout.decode("utf-8", "replace").replace("\r\n", "\n").replace("\r", "\n")
        return out, squeeze(out, "pdf")
    with open(path, encoding="utf-8", errors="replace") as f:
        raw = f.read()
    if ext in (".html", ".htm"):
        raw = html_text(raw)
        return raw, squeeze(raw, "html")
    if ext == ".md":
        raw = drop_comments(raw)
    return raw, squeeze(raw, "plain")


def to_text(path):
    """Plain text of one document, squeezed."""
    return convert(path)[1]


# ---------------------------------------------------------------- sets

def expand(name, defs, catalog, stack=()):
    """Document ids of a configured set: includes first, then its own docs, globs expanded."""
    if name in stack:
        raise ConfigError("sets include each other in a loop: " + " -> ".join(stack + (name,)))
    spec = defs[name]
    if not isinstance(spec, dict):
        raise ConfigError(f"[sets.{name}] must be a table")
    out = []
    for inc in str_list(spec.get("include"), f"[sets.{name}] include"):
        if inc not in defs:
            raise ConfigError(f"set '{name}' includes unknown set '{inc}'")
        out += expand(inc, defs, catalog, stack + (name,))
    for pat in str_list(spec.get("docs"), f"[sets.{name}] docs"):
        hits = [d for d in catalog if fnmatch.fnmatchcase(d, pat)] if re.search(r"[*?\[]", pat) else []
        out += hits or [pat]  # no hit stays in the list and shows up as MISSING
    return list(dict.fromkeys(out))


def build_sets(cfg, catalog, conf, names):
    """(sets, kinds) for everything that may be asked for.

    kinds: 'set' (from config), 'all' (whole library) or 'custom' (user ticks from the library).
    """
    defs = cfg.get("sets", {})
    if not isinstance(defs, dict):
        raise ConfigError("[sets] must be a table")
    sets = {n: expand(n, defs, catalog) for n in defs}
    kinds = {n: "set" for n in defs}
    sets["all"], kinds["all"] = list(catalog), "all"
    for n in list(conf) + list(names):
        if n in sets:
            continue
        if n in conf and not (isinstance(conf[n], dict) and conf[n].get("custom")):
            continue  # saved choice of a set that no longer exists
        if n not in conf and not NAME_RE.fullmatch(n):
            continue
        sets[n], kinds[n] = list(catalog), "custom"
    return sets, kinds


def store_name(d):
    return d.replace("/", " - ") + ".txt"


def id_hash(d):
    """FNV-1a (32 bit) of the id's UTF-8 bytes; the same number in the Rust version."""
    h = 0x811C9DC5
    for b in d.encode("utf-8"):
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h


def free_store_name(d, index):
    """store_name, unless another document already uses that file (`a/b` and `a - b`) or the name
    is too long for the file system: then the id's hash goes into the name, so two documents
    never share one copy."""
    fn = store_name(d)
    taken = any(i != d and isinstance(r, dict) and r.get("file") == fn for i, r in index.items())
    if not taken and len(fn.encode("utf-8")) <= LONGEST_NAME:
        return fn
    stem = d.replace("/", " - ").encode("utf-8")[:LONGEST_NAME - 15].decode("utf-8", "ignore")
    return f"{stem} ({id_hash(d):08x}).txt"


LONGEST_NAME = 200  # bytes; room for the .PID.tmp while writing; the Rust version uses the same


class Texts:
    """Lazy {doc id: plain text}, kept as a persistent copy in the researcher's own folder.

    A document is converted once and again only when its source file changed (size, mtime) or
    TEXT_VERSION went up. The library itself is only ever read. `d in texts` is False for
    documents that cannot be read; copies of documents that left the library stay.
    """

    def __init__(self, paths, store=None):
        self.paths, self.store, self.cache, self.errors = paths, store, {}, {}
        self.raw = {}
        self.index, self.dirty = {}, False
        if store:
            try:
                with open(os.path.join(store, ".index.json"), encoding="utf-8") as f:
                    self.index = json.load(f)
            except (OSError, ValueError):
                self.index = {}
            if not isinstance(self.index, dict):
                self.index = {}

    def _stored(self, d):
        """Text of a fresh stored copy, or None."""
        rec = self.index.get(d)
        try:
            st = os.stat(self.paths[d])
            if (isinstance(rec, dict) and rec.get("v") == TEXT_VERSION
                    and rec.get("src") == self.paths[d] and rec.get("size") == st.st_size
                    and rec.get("mtime_ns") == st.st_mtime_ns):
                with open(os.path.join(self.store, rec["file"]), encoding="utf-8") as f:
                    head, text = f.read().split("\n\n", 1)
                if head == f"SOURCE: {self.paths[d]}":  # else: another document's copy
                    return text.removesuffix("\n")
        except (OSError, KeyError, ValueError, TypeError):
            pass
        return None

    def raw_chars(self, d):
        """Characters of the converted text before squeezing (the whole copy is read once)."""
        return self.raw.get(d, len(self.cache.get(d, "")))

    def _save(self, d, text, raw, st):
        """`st` is the source as it was before converting: if it changes meanwhile, the next
        run sees another size or mtime and converts it again."""
        os.makedirs(self.store, exist_ok=True)
        fn = free_store_name(d, self.index)
        tmp = os.path.join(self.store, f"{fn}.{os.getpid()}.tmp")
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(f"SOURCE: {self.paths[d]}\n\n{text}\n")
        os.replace(tmp, os.path.join(self.store, fn))
        self.index[d] = {"file": fn, "src": self.paths[d], "size": st.st_size,
                         "mtime_ns": st.st_mtime_ns, "raw": raw, "v": TEXT_VERSION}
        self.dirty = True

    def __contains__(self, d):
        if d in self.cache:
            return True
        if d not in self.paths or d in self.errors:
            return False
        try:
            text = self._stored(d) if self.store else None
            if text is None:
                st = os.stat(self.paths[d])
                raw, text = convert(self.paths[d])
                if self.store:
                    self._save(d, text, len(raw), st)
            self.cache[d] = text
            self.raw[d] = (self.index.get(d) or {}).get("raw", len(text))
        except (OSError, RuntimeError) as e:
            self.errors[d] = str(e)
            return False
        return True

    def __getitem__(self, d):
        if d not in self:
            raise KeyError(d)
        return self.cache[d]

    def get(self, d, default=""):
        return self.cache[d] if d in self else default

    def path(self, d):
        """The stored .txt of a readable document."""
        return os.path.join(self.store, self.index[d]["file"])

    def flush(self):
        if self.store and self.dirty:
            tmp = os.path.join(self.store, f".index.json.{os.getpid()}.tmp")
            try:
                with open(tmp, "w", encoding="utf-8") as f:
                    json.dump(self.index, f, indent=1, sort_keys=True)
                os.replace(tmp, os.path.join(self.store, ".index.json"))
            except OSError:
                return  # the copies are still valid; the next run just checks them again
            self.dirty = False


def sections_for(names, sets, kinds, conf, texts, setup):
    """One section per requested set.

    docs     what the set is made of (for a custom set: the whole library)
    main     documents shown at the top: the set's own plus extras saved earlier
    other    the rest of the library, offered as "add from your library"
    ticked   pre-ticked documents; new = added since the preset was saved
    """
    catalog = sets["all"]
    out = []
    for n in names:
        docs = sets[n]
        saved = conf.get(n)
        if saved is not None and not isinstance(saved, dict):
            saved = {}  # a damaged entry counts as saved with nothing in it, as in the Rust version
        known = set(saved.get("known", [])) if saved else set()
        selected = set(saved.get("selected", [])) if saved else set()
        new = {d for d in docs if d not in known} if saved else set()
        if kinds[n] == "custom":  # nothing ticked on first use, new docs never auto-ticked
            ticked = selected & set(docs)
            remember = saved is None or bool(new) or setup
            main = [d for d in docs if d in ticked or d in new] if saved else []
        else:
            ticked = set(docs) if saved is None else (selected | new)
            remember = saved is None or bool(new) or setup
            main = docs + [d for d in catalog if d in selected and d not in docs]
        other = [d for d in catalog if d not in set(main)]
        out.append({"name": n, "kind": kinds[n], "docs": docs, "main": main, "other": other,
                    "new": new, "ticked": {d for d in ticked if d in texts},  # unreadable: no
                    "remember": remember, "first": saved is None})
    return out


def resolve_names(argv_names, cfg):
    al = cfg.get("aliases")
    aliases = {k.lower(): v for k, v in al.items() if isinstance(v, str)} if isinstance(al, dict) else {}
    sets = cfg.get("sets", {}) if isinstance(cfg.get("sets"), dict) else {}
    defined = {k.lower(): k for k in reversed(list(sets))}  # `[sets.MyRust]` is reached by `myrust`

    def canon(n):
        return n if n in sets else defined.get(n.lower(), n)
    names = [canon(aliases.get(n.lower(), n.lower())) for n in argv_names]
    if not names:
        d = cfg.get("default_set")
        names = [canon(d)] if isinstance(d, str) and d else []
    return list(dict.fromkeys(names))


# ---------------------------------------------------------------- saved choices

def set_aside(path):
    """A state file that exists but does not parse is kept as PATH.bad before the next save
    overwrites it with an empty state."""
    if os.path.isfile(path):
        try:
            os.replace(path, path + ".bad")
        except OSError:
            return
        print(f"NOTE: {path} could not be read; kept as {path}.bad, starting empty", file=sys.stderr)


def load_conf(path):
    try:
        with open(path, "rb") as f:
            data = f.read()
    except FileNotFoundError:
        return {}
    except OSError:  # unreadable (permissions, a folder): keep it aside instead of saving over it
        set_aside(path)
        return {}
    try:
        conf = json.loads(data)
    except ValueError:
        conf = None
    if isinstance(conf, dict):
        return conf
    set_aside(path)
    return {}


def save_conf(path, conf):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    write_atomic(path, json.dumps(conf, indent=1, ensure_ascii=False))


# ---------------------------------------------------------------- time bands, names, window state

CHIPS = ("day", "week", "month", "all")
CHIP_LABELS = {"day": "Day", "week": "Week", "month": "Month", "all": "All"}
CHIP_HOURS = {"day": 24, "week": 7 * 24, "month": 30 * 24}


def chip_holds(chip, now, t):
    """True when `t` falls inside the chip: the last 24 hours, 7 days or 30 days (or always)."""
    return chip == "all" or now - t < datetime.timedelta(hours=CHIP_HOURS[chip])


def band(now, t):
    """The heading a time sits under (aware datetimes): Today and Yesterday follow local
    midnight, the rest the age (7 and 30 days). A time in the future counts as Today."""
    day = now.date()
    if now.astimezone().utcoffset() == now.utcoffset():
        # local time: each midnight with its own offset, so a clock change cannot shift a day
        midnight, yesterday = (datetime.datetime.combine(d, datetime.time.min).astimezone()
                               for d in (day, day - datetime.timedelta(days=1)))
    else:  # a fixed offset that is not this machine's: no clock changes to follow
        midnight = now.replace(hour=0, minute=0, second=0, microsecond=0)
        yesterday = midnight - datetime.timedelta(days=1)
    if t >= midnight:
        return "Today"
    if t >= yesterday:
        return "Yesterday"
    age = now - t
    if age < datetime.timedelta(days=7):
        return "Earlier this week"
    return "Earlier this month" if age < datetime.timedelta(days=30) else "Older"


def parse_iso(t):
    """Seconds since the epoch of an ISO time (2026-11-16T10:00:00Z), or None."""
    try:
        return int(datetime.datetime.fromisoformat(t.replace("Z", "+00:00")).timestamp())
    except (AttributeError, TypeError, ValueError):
        return None


def local_time(secs):
    return datetime.datetime.fromtimestamp(secs).astimezone()


def state_names(path, key):
    """{id: name} under `key` ("docs" or "sets") of names.json."""
    rec = load_conf(path).get(key)
    return {k: v for k, v in rec.items() if isinstance(v, str)} if isinstance(rec, dict) else {}


def state_rename(path, key, changes):
    """Apply renames under `key` ("" removes the name) and save."""
    if not changes:
        return
    allnames = load_conf(path)
    m = allnames.get(key) if isinstance(allnames.get(key), dict) else {}
    for k, v in changes.items():
        if v:
            m[k] = v
        else:
            m.pop(k, None)
    allnames[key] = m
    save_conf(path, allnames)


def ui_state(path, chats):
    """(chip, group by time) last used in a tab; first run: chats on Week by time, research on
    All by folder."""
    rec = load_conf(path).get("chats" if chats else "research") or {}
    chip = rec.get("chip") if rec.get("chip") in CHIPS else ("week" if chats else "all")
    group = rec.get("group")
    return chip, (group == "time") if group in ("time", "folder") else chats


def save_ui(path, chats, chip, by_time):
    allui = load_conf(path)
    allui["chats" if chats else "research"] = {"chip": chip, "group": "time" if by_time else "folder"}
    save_conf(path, allui)


def profile_presets(path, profile):
    """The presets of a project profile in profiles.json, in saved order: [(name, {"code" |
    "chats" | "research": [row ids]})]."""
    presets = (load_conf(path).get(profile) or {}).get("presets")
    if not isinstance(presets, dict):
        return []
    return [(n, {k: [x for x in (v or {}).get(k, []) if isinstance(x, str)] for k in ("code", "chats", "research")})
            for n, v in presets.items()]


def save_profile_preset(path, profile, preset, lists):
    """Save (or replace) one preset of a profile."""
    allp = load_conf(path)
    entry = allp.get(profile)
    if not isinstance(entry, dict) or not isinstance(entry.get("presets"), dict):
        entry = allp[profile] = {"presets": {}}
    entry["presets"][preset] = lists
    save_conf(path, allp)


CODE_SKIP_DIRS = ("target", "node_modules", "__pycache__", "dist", "build", "venv")
CODE_MAX_BYTES = 1_000_000


def code_scan(root, globs):
    """Text files under `root` whose relative path matches one of `globs` (`*` crosses folders):
    {id: (path, bytes, mtime)}. Hidden files and folders, symlinks, binaries and files over 1 MB
    are left out."""
    out = {}
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if not d.startswith(".") and d not in CODE_SKIP_DIRS
                       and not os.path.islink(os.path.join(dirpath, d))]
        rel = os.path.relpath(dirpath, root).replace(os.sep, "/")
        for f in filenames:
            path = os.path.join(dirpath, f)
            doc = f if rel == "." else f"{rel}/{f}"
            if f.startswith(".") or os.path.islink(path):
                continue
            try:
                st = os.stat(path)
                if not stat.S_ISREG(st.st_mode):
                    continue  # a pipe or socket would block the read below
                if st.st_size > CODE_MAX_BYTES or not any(fnmatch.fnmatchcase(doc, g) for g in globs):
                    continue
                with open(path, "rb") as fh:
                    if b"\0" in fh.read(8192):
                        continue
            except OSError:
                continue
            out[doc] = (path, st.st_size, int(st.st_mtime))
    return dict(sorted(out.items()))


def bad_preset_name(secs, taken):
    """Why a remembered chat preset name cannot be saved, or None."""
    seen = set()
    for s in secs:
        if not s["remember"]:
            continue
        n = s["save_as"].strip()
        if not PRESET_RE.fullmatch(n):
            return f'"{n}" cannot be a preset name: use lowercase letters, digits, . _ -'
        if (n != s["name"] and n in taken) or n in seen:
            return f'a preset named "{n}" exists already: pick another name'
        seen.add(n)
    return None


# ---------------------------------------------------------------- popup


def host_name():
    """The program that asked for the load, for the popup text: RESEARCHER_HOST if set, opencode when
    opencode started us (it sets OPENCODE=1 for its tools), otherwise Claude Code."""
    over = (os.environ.get("RESEARCHER_HOST") or "").strip()
    if over:
        return over
    return "opencode" if "OPENCODE" in os.environ else "Claude Code"


def subtitle(sec, chats):
    if chats:
        if sec["first"]:
            return ("new preset: nothing saved yet. Tick the chats this one needs; tick Remember to "
                    "keep them as its preset.")
        return f"saved preset: {len(sec['ticked'])} chats" + (
            f", {len(sec['new'])} newer chats since (marked NEW)" if sec["new"] else "")
    if sec["first"]:
        return ("new set: nothing saved yet. Tick the research this one needs; it is saved as its "
                "preset." if sec["kind"] == "custom" else
                f"{len(sec['docs'])} documents (no preset saved yet, all ticked)")
    return f"saved preset: {len(sec['ticked'] - sec['new'])} documents" + (
        f", {len(sec['new'])} new in the library" if sec["new"] else "")


def tab_state(tab):
    """The window's working copy of one tab: rows as small dicts."""
    look, sizes = tab["look"], tab["sizes"]
    chats, code = bool(look.get("chats")), bool(look.get("code"))
    labels, defaults = look.get("labels", {}), look.get("defaults", {})
    notes, groups, times = look.get("notes", {}), look.get("groups", {}), look.get("times", {})
    titles = look.get("titles", {})

    def row(d, sec):
        f = d.rpartition("/")[0]
        return {"doc": d, "label": labels.get(d), "default": defaults.get(d), "note": notes.get(d, ""),
                "folder": groups[d] if d in groups else (f + "/" if f else "(top level)"),
                "when": times.get(d), "on": d in sizes and d in sec["ticked"], "readable": d in sizes,
                "new": d in sec["new"]}
    secs = [{"name": s["name"], "title": titles.get(s["name"], ""), "title0": titles.get(s["name"], ""),
             "save_as": s["name"],
             "sub": (f"{len(s['docs'])} " + ("files under the profile root" if code else "chats" if chats
                                              else "documents in your library")
                     + "; a profile preset above ticks them") if look.get("no_remember") else subtitle(s, chats),
             "main": [row(d, s) for d in s["main"]], "other": [row(d, s) for d in s["other"]],
             "other_open": not s["main"], "saved": set(s["ticked"]), "first": s["first"],
             "remember": s["remember"]} for s in tab["sections"]]
    return {"chats": chats, "code": code, "no_remember": bool(look.get("no_remember")),
            "secs": secs, "sizes": sizes, "filter": "",
            "chip": look.get("chip", "all"), "by_time": bool(look.get("by_time")), "timed": bool(times),
            "renames": {}, "taken": set(look.get("taken", ()))}


def row_shown(st, r, short):
    """A rename made in the window, else the saved label, else from the id (`short`: its last
    part, under its folder heading)."""
    from_id = r["doc"].rpartition("/")[2] if short else r["doc"]
    if r["doc"] in st["renames"]:
        return st["renames"][r["doc"]] or r["default"] or from_id
    return r["label"] or from_id


def tab_rows(st):
    return [r for s in st["secs"] for r in s["main"] + s["other"]]


def in_chip(st, r, now):
    return st["chip"] == "all" or (r["when"] is not None and chip_holds(st["chip"], now, local_time(r["when"])))


def tab_chosen(st):
    return {r["doc"] for r in tab_rows(st) if r["on"]}


def tab_hidden(st, now):
    """Ticked rows the chip hides (they are still loaded)."""
    return len({r["doc"] for r in tab_rows(st) if r["on"] and not in_chip(st, r, now)})


def chip_count(st, chip, now):
    whens = {r["doc"]: r["when"] for r in tab_rows(st)}
    return sum(1 for w in whens.values()
               if chip == "all" or (w is not None and chip_holds(chip, now, local_time(w))))


def tab_name(st_or_kind):
    return "Code" if st_or_kind.get("code") else "Chats" if st_or_kind.get("chats") else "Research"


def tab_noun(st, short=False):
    return "files" if st["code"] else "chats" if st["chats"] else ("docs" if short else "documents")


def popup(tabs, start, setup, command="", profile=None):
    """Show the chooser on tab `start` and wait. Returns one pick per tab ({"picks": [{name, ticked,
    remember, save_as, title}], "renames", "chip", "by_time"}), None if cancelled or "timeout".
    Raises RuntimeError when no display/GTK 4 is available.

    A tab is {"sections", "sizes": {id: chars} (readable rows only), "look": {"chats", "labels",
    "defaults", "notes", "groups", "times", "titles", "taken", "chip", "by_time"}}, or
    {"chats": bool, "code": bool, "loader": fn} for a tab that loads only when it is first clicked (fn returns
    a tab or None; it runs in a thread so the window keeps drawing). A tab never opened gives
    None in the result.

    profile: {"name", "presets": [(name, [row ids per tab])], "selected": index or None}: a row of
    preset buttons that tick every tab at once, and "save these ticks as the profile preset".
    The result is then (picks, preset name to save or None)."""
    try:
        import gi
        gi.require_version("Gtk", "4.0")
        from gi.repository import GLib, Gtk
    except (ImportError, ValueError) as e:
        raise RuntimeError(f"GTK 4 not available ({e})")
    if not Gtk.init_check():
        raise RuntimeError("no display")
    Gtk.Settings.get_default().set_property("gtk-application-prefer-dark-theme", True)
    esc = lambda t: GLib.markup_escape_text(t, -1)
    loop = GLib.MainLoop()
    result = {"value": None}
    import threading
    states = [None if "loader" in t else tab_state(t) for t in tabs]
    kinds = [{"chats": bool(t["chats"]), "code": bool(t.get("code"))} if "loader" in t else
             {"chats": bool(t["look"].get("chats")), "code": bool(t["look"].get("code"))} for t in tabs]
    want = [None] * len(tabs)  # ticks a profile preset asked for before the tab was loaded
    prof = {"picked": None, "save": False, "name": ""}
    loaders = [t.get("loader") for t in tabs]
    loading = [False] * len(tabs)
    cur = {"i": min(start, len(states) - 1), "edit": None}  # edit: (tab, section, row id or "", text)
    now = datetime.datetime.now().astimezone()
    host = host_name()

    def clear(box):
        while (c := box.get_first_child()) is not None:
            box.remove(c)

    if len(tabs) == 1:
        what = "choose chats to load" if kinds[0]["chats"] else "choose research to load"
    else:
        what = "choose code, chats and research to load" if profile else "choose chats and research to load"
    win = Gtk.Window(title="The Researcher - " + ("change saved presets" if setup else what))
    win.set_default_size(760, 800)
    outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
    for m in ("top", "bottom", "start", "end"):
        getattr(outer, f"set_margin_{m}")(14)
    if profile:
        prow = Gtk.Box(spacing=6)
        plabel = Gtk.Label()
        plabel.set_markup(f"<big><b>Profile {esc(profile['name'])}:</b></big>")
        prow.append(plabel)
        preset_buttons = []
        pgroup = None
        for i, (pname, _) in enumerate(profile["presets"]):
            b = Gtk.ToggleButton(label=pname)
            if pgroup is not None:
                b.set_group(pgroup)
            pgroup = pgroup or b
            b.connect("toggled", lambda b, i=i: b.get_active() and apply_preset(i))
            prow.append(b)
            preset_buttons.append(b)
        if not profile["presets"]:
            hint = Gtk.Label()
            hint.set_markup("<small><span alpha='60%'>no presets yet: tick what you need, then save it below</span></small>")
            prow.append(hint)
        outer.append(prow)
    tabbar = Gtk.Box(spacing=6)
    tab_buttons = []
    if len(states) > 1:
        group = None
        for i, st in enumerate(states):
            b = Gtk.ToggleButton()
            if group is not None:
                b.set_group(group)
            group = group or b
            b.set_active(i == cur["i"])
            b.connect("toggled", lambda b, i=i: b.get_active() and switch(i))
            tabbar.append(b)
            tab_buttons.append(b)
        outer.append(tabbar)
    head = Gtk.Label(xalign=0, wrap=True)
    outer.append(head)
    chips = Gtk.Box(spacing=6)
    outer.append(chips)
    search = Gtk.SearchEntry()
    outer.append(search)
    scroll = Gtk.ScrolledWindow(vexpand=True)
    body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
    scroll.set_child(body)
    outer.append(scroll)
    total = Gtk.Label(xalign=0)
    outer.append(total)
    if profile:
        srow = Gtk.Box(spacing=6)
        save_cb = Gtk.CheckButton(label="Save these ticks as the profile preset")
        save_entry = Gtk.Entry(width_chars=16, tooltip_text="An existing name is replaced")
        save_cb.connect("toggled", lambda b: (prof.__setitem__("save", b.get_active()), update()))
        save_entry.connect("changed", lambda e: (prof.__setitem__("name", e.get_text()), update()))
        srow.append(save_cb)
        srow.append(save_entry)
        outer.append(srow)
    error = Gtk.Label(xalign=0, wrap=True)
    outer.append(error)
    bar = Gtk.Box(spacing=8, halign=Gtk.Align.END)
    cancel = Gtk.Button(label="Close without saving" if setup else "Cancel (load nothing)")
    ok = Gtk.Button(label="Load")
    ok.add_css_class("suggested-action")
    bar.append(cancel)
    bar.append(ok)
    outer.append(bar)
    win.set_child(outer)

    def later(fn):
        GLib.idle_add(lambda: (fn(), False)[1])

    def update(*_):
        parts, short = [], []
        k = n = hidden = 0
        for st in (st for st in states if st):
            c = tab_chosen(st)
            k += sum(st["sizes"].get(d, 0) for d in c) / CHARS_PER_K
            n += len(c)
            hidden += tab_hidden(st, now)
            if c or len(states) == 1:
                parts.append(f"{len(c)} {tab_noun(st)}")
                short.append(f"{len(c)} {tab_noun(st, True)}")
        line = f"{' + '.join(parts) or 'nothing ticked'}, ~{k:.0f}k tokens" + ("" if setup else " will be loaded")
        if hidden:
            line += f" ({hidden} ticked but hidden by the time chip)"
        total.set_markup(f"<b>{esc(line)}</b>")
        bad = next((b for st in states if st and st["chats"] and not st["no_remember"]
                    for b in [bad_preset_name(st["secs"], st["taken"])] if b), None)
        if profile and prof["save"] and not PRESET_RE.fullmatch(prof["name"].strip()):
            bad = ('"' + prof["name"].strip() + '" cannot be a preset name: '
                   'use lowercase letters, digits, . _ -')
        error.set_markup(f"<span foreground='#ff6666'>{esc(bad)}</span>" if bad else "")
        if setup:
            ok.set_label("Save presets")
        elif len(states) == 1:
            ok.set_label("Load " + "".join(parts))
        else:
            ok.set_label("Load " + " + ".join(short) if short else "Load")
        ok.set_sensitive((setup or n > 0) and not bad and cur["edit"] is None and not any(loading))
        for i, b in enumerate(tab_buttons):
            st = states[i]
            status = (f"{len(tab_chosen(st))} ticked" if st else "loading…" if loading[i]
                      else "click to load" if loaders[i] else "nothing to show")
            b.set_label(f"{tab_name(kinds[i])} ({status})")

    def visible(st, r, short):
        if not in_chip(st, r, now):
            return False
        q = st["filter"]
        return not q or q in f"{row_shown(st, r, short)} {r['folder']} {r['doc']}".lower()

    def start_edit(si, doc, text):
        cur["edit"] = (cur["i"], si, doc, text)
        later(render)

    def commit(si, doc, entry):
        if cur["edit"] is None or cur["edit"][:3] != (cur["i"], si, doc):
            return  # already done (Enter, then the entry lost focus)
        text = entry.get_text().strip()
        st = states[cur["i"]]
        if doc:
            st["renames"][doc] = text
        else:
            st["secs"][si]["title"] = text
        cur["edit"] = None
        later(render)

    def edit_entry(si, doc, text, hint):
        e = Gtk.Entry(text=text, placeholder_text=hint, width_chars=40)
        e.connect("activate", lambda e: commit(si, doc, e))
        focus = Gtk.EventControllerFocus()
        focus.connect("leave", lambda *_: commit(si, doc, e))
        e.add_controller(focus)
        later(e.grab_focus)
        return e

    def add_rows(target, st, si, rows, indent):
        by_time = st["by_time"]
        folder_heads = not by_time and (st["chats"] or len({r["folder"] for r in rows}) > 1)
        short = folder_heads and not st["chats"]
        order = sorted(rows, key=lambda r: r["when"] if r["when"] is not None else float("-inf"),
                       reverse=True) if by_time else rows  # stable: ties keep their order
        last = None
        for r in order:
            if not visible(st, r, short):
                continue
            if by_time:
                head = band(now, local_time(r["when"])) if r["when"] is not None else "Unknown time"
            else:
                head = r["folder"] if folder_heads else None
            if head is not None and head != last:
                h = Gtk.Label(xalign=0)
                h.set_markup(f"<small><b>{esc(head)}</b></small>")
                h.set_margin_top(6)
                h.set_margin_start(indent)
                target.append(h)
                last = head
            line = Gtk.Box(spacing=6)
            line.set_margin_start(indent + (12 if folder_heads or by_time else 0))
            shown = row_shown(st, r, short)
            ed = cur["edit"]
            if ed and ed[:3] == (cur["i"], si, r["doc"]):
                line.append(edit_entry(si, r["doc"], ed[3], "empty = the first prompt again" if st["chats"]
                                       else "empty = the file name again"))
                hint = Gtk.Label()
                hint.set_markup("<small><span alpha='60%'>Enter keeps it, Esc undoes</span></small>")
                line.append(hint)
            elif r["readable"]:
                cb = Gtk.CheckButton()
                l = Gtk.Label(xalign=0)
                bits = [esc(shown), f"<small>~{st['sizes'].get(r['doc'], 0) / CHARS_PER_K:.0f}k</small>"]
                if r["note"]:
                    bits.append(f"<small><span alpha='60%'>{esc(r['note'])}</span></small>")
                if by_time and st["chats"]:  # the folder heading moves onto the row
                    bits.append(f"<small><span alpha='60%'>{esc(r['folder'])}</span></small>")
                if not st["chats"] and shown not in (r["doc"], r["doc"].rpartition("/")[2]) and shown:
                    bits.append(f"<small><span alpha='60%'><tt>{esc(r['doc'])}</tt></span></small>")
                if r["new"]:
                    bits.append("<span foreground='#ffcc33'><b>NEW</b></span>")
                l.set_markup("  ".join(bits))
                cb.set_child(l)
                cb.set_active(r["on"])
                cb.connect("toggled", lambda cb, r=r: (r.__setitem__("on", cb.get_active()), update()))
                line.append(cb)
                if not st["code"]:  # source files keep their path
                    twice = Gtk.GestureClick()
                    twice.connect("pressed", lambda _g, n, _x, _y, si=si, d=r["doc"], s=shown:
                                  n == 2 and start_edit(si, d, s))
                    cb.add_controller(twice)
                    pen = Gtk.Button(label="rename", tooltip_text="Rename (or double-click the name)")
                    pen.add_css_class("flat")
                    pen.connect("clicked", lambda _b, si=si, d=r["doc"], s=shown: start_edit(si, d, s))
                    line.append(pen)
            else:
                l = Gtk.Label(xalign=0)
                l.set_markup(f"<s>{esc(shown)}</s>  <span foreground='#ff6666'>MISSING or unreadable</span>")
                line.append(l)
            target.append(line)

    def render():
        st = states[cur["i"]]
        if st is None:  # a tab that loads on its first click: loading, or nothing in it
            clear(chips)
            clear(body)
            if loading[cur["i"]]:
                what = "earlier chats (the first time can take a while)" if kinds[cur["i"]]["chats"] else "research"
                head.set_markup(f"<big><b>Loading {what}…</b></big>")
            else:
                head.set_markup("<big><b>" + ("No earlier chats to show." if kinds[cur["i"]]["chats"] else
                                "No research to show: set `default_set` (and a library) in the config.")
                                + "</b></big>")
            update()
            return
        names = ", ".join(f"<b>{esc(s['name'])}</b>" for s in st["secs"])
        if setup:
            head.set_markup(f"<big><b>Change the saved presets</b></big> for {names}\n"
                            "Tick what each should contain, then press <b>Save presets</b>. Nothing is loaded.")
        else:
            whatnow = "source files" if st["code"] else "earlier chats" if st["chats"] else "research"
            head.set_markup(f"<big><b>{esc(host)} wants to load {whatnow}</b></big> for {names}\n"
                            "Tick what this session needs, then press <b>Load</b>. Nothing is sent to "
                            f"{esc('Claude' if host == 'Claude Code' else host)} until then; "
                            "<b>Cancel</b> loads nothing."
                            + (f"\n<small>Asked by: <tt>{esc(command)}</tt></small>" if command else ""))
        search.set_placeholder_text("Filter: words from the path" if st["code"] else
                                    "Filter: words from the name, the first prompt or the folder"
                                    if st["chats"] else "Filter: words from the name or the id")
        clear(chips)
        if st["timed"]:
            group = None
            for chip in CHIPS:
                b = Gtk.ToggleButton(label=f"{CHIP_LABELS[chip]} ({chip_count(st, chip, now)})")
                if group is not None:
                    b.set_group(group)
                group = group or b
                b.set_active(st["chip"] == chip)
                b.connect("toggled", lambda b, c=chip: b.get_active() and set_tab(chip=c))
                chips.append(b)
            chips.append(Gtk.Label(label="   Group by:"))
            tb = Gtk.ToggleButton(label="time")
            fb = Gtk.ToggleButton(label="folder", group=tb)
            (tb if st["by_time"] else fb).set_active(True)
            tb.connect("toggled", lambda b: b.get_active() and set_tab(by_time=True))
            fb.connect("toggled", lambda b: b.get_active() and set_tab(by_time=False))
            chips.append(tb)
            chips.append(fb)
        clear(body)
        narrowed = bool(st["filter"]) or st["chip"] != "all"
        for si, sec in enumerate(st["secs"]):
            top = Gtk.Box(spacing=6)
            top.set_margin_top(12)
            ed = cur["edit"]
            if ed and ed[:3] == (cur["i"], si, ""):
                top.append(edit_entry(si, "", ed[3], "empty = its own name again"))
            else:
                lbl = Gtk.Label(xalign=0)
                small = f"  <small><span alpha='60%'><tt>{esc(sec['name'])}</tt></span></small>" if sec["title"] else ""
                lbl.set_markup(f"<big><b>{esc(sec['title'] or sec['name'])}</b></big>{small}")
                top.append(lbl)
                if not st["chats"] and not st["code"]:
                    pen = Gtk.Button(label="rename", tooltip_text="Give this set a display name")
                    pen.add_css_class("flat")
                    pen.connect("clicked", lambda _b, si=si, t=sec["title"]: start_edit(si, "", t))
                    top.append(pen)
            body.append(top)
            sub = Gtk.Label(xalign=0, wrap=True)
            sub.set_markup(f"<small>{esc(sec['sub'])}</small>")
            body.append(sub)
            row = Gtk.Box(spacing=6)

            def pick(sec, on):
                for r in sec["main"] + sec["other"]:
                    short = not st["chats"]
                    if visible(st, r, short) or visible(st, r, False):
                        r["on"] = on and r["readable"]
                later(render)

            def back(sec):
                for r in sec["main"] + sec["other"]:
                    r["on"] = r["doc"] in sec["saved"]
                later(render)
            buttons = [("Select shown" if narrowed else "Select all", lambda _b, s=sec: pick(s, True)),
                       ("Unselect shown" if narrowed else "Select none", lambda _b, s=sec: pick(s, False))]
            if not st["no_remember"]:  # a profile tab has no saved preset of its own
                buttons.append(("Back to saved preset", lambda _b, s=sec: back(s)))
            for text, fn in buttons:
                b = Gtk.Button(label=text)
                b.connect("clicked", fn)
                if text == "Back to saved preset":
                    b.set_sensitive(not sec["first"])
                row.append(b)
            body.append(row)
            add_rows(body, st, si, sec["main"], 12)
            if sec["other"]:
                inner = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
                if st["chats"]:
                    label = ("Your chats: tick the ones to load" if not sec["main"] else
                             f"Add other chats ({len(sec['other'])} more)")
                else:
                    label = ("Your library: tick the research to use" if not sec["main"] else
                             f"Add other research from your library ({len(sec['other'])} more)")
                ex = Gtk.Expander(label=label)
                ex.set_margin_start(12)
                ex.set_margin_top(6)
                ex.set_expanded(sec["other_open"] or bool(st["filter"]))
                ex.connect("notify::expanded", lambda ex, _p, s=sec: s.__setitem__("other_open", ex.get_expanded())
                           if not st["filter"] else None)
                ex.set_child(inner)
                body.append(ex)
                add_rows(inner, st, si, sec["other"], 0)
            if st["no_remember"]:
                continue
            rem = Gtk.Box(spacing=6)
            rem.set_margin_start(12)
            rem.set_margin_top(6)
            rb = Gtk.CheckButton(label="Remember this choice as the preset" if st["chats"]
                                 else f"Remember this choice as the preset for {sec['name']}")
            rb.set_active(sec["remember"])
            rb.set_sensitive(not setup)
            rb.connect("toggled", lambda b, s=sec: (s.__setitem__("remember", b.get_active()), later(render)))
            rem.append(rb)
            if st["chats"] and (sec["remember"] or setup):
                rem.append(Gtk.Label(label="named"))
                e = Gtk.Entry(text=sec["save_as"], width_chars=16,
                              tooltip_text="Renaming moves the saved ticks to the new name")
                e.connect("changed", lambda e, s=sec: (s.__setitem__("save_as", e.get_text()), update()))
                rem.append(e)
            body.append(rem)
        update()

    def set_tab(**kw):
        if states[cur["i"]] is None:
            return
        states[cur["i"]].update(kw)
        later(render)

    def open_tab(i):
        """Load a tab that waited for its first click."""
        load, loaders[i] = loaders[i], None
        if load is None:
            return
        loading[i] = True

        def loaded(tab):
            loading[i] = False
            states[i] = tab_state(tab) if tab else None
            if states[i] is not None and want[i] is not None:
                tick_only(states[i], want[i])
                want[i] = None
            if not cur.get("closed"):
                render()

        def work():
            try:
                tab = load()
            except Exception:  # noqa: BLE001 - a failed load shows as "nothing to show"
                tab = None
            GLib.idle_add(lambda: (loaded(tab), False)[1])
        threading.Thread(target=work, daemon=True).start()

    def tick_only(st, ids):
        for r in tab_rows(st):
            r["on"] = r["readable"] and r["doc"] in ids

    def apply_preset(i):
        """Tick what profile preset i holds, in every tab (loading the tabs it needs)."""
        pname, lists = profile["presets"][i]
        for j, ids in enumerate(lists):
            if states[j] is not None:
                tick_only(states[j], set(ids))
            else:
                if ids:
                    open_tab(j)
                want[j] = set(ids)
        prof["picked"] = i
        save_entry.set_text(pname)
        later(render)

    def switch(i):
        cur["i"], cur["edit"] = i, None
        open_tab(i)
        search.set_text(states[i]["filter"] if states[i] else "")
        later(render)

    def finish(value):
        result["value"] = value
        cur["closed"] = True
        win.destroy()
        loop.quit()

    def done(*_):
        picks = [{"picks": [{"name": s["name"], "ticked": {r["doc"] for r in s["main"] + s["other"] if r["on"]},
                            "remember": s["remember"], "save_as": s["save_as"].strip(),
                            "title": s["title"].strip() if s["title"].strip() != s["title0"] else None}
                           for s in st["secs"]],
                 "renames": dict(st["renames"]), "chip": st["chip"], "by_time": st["by_time"]} if st else None
                 for st in states]
        if profile:
            finish((picks, prof["name"].strip() if prof["save"] else None))
        else:
            finish(picks)

    def refilter(*_):
        if states[cur["i"]] is None:
            return
        states[cur["i"]]["filter"] = search.get_text().strip().lower()
        render()

    def key(_c, kv, _kc, _s):
        if kv != 0xff1b:  # Escape: undoes a rename in progress, otherwise it is Cancel
            return False
        if cur["edit"] is not None:
            cur["edit"] = None
            later(render)
        else:
            finish(None)
        return True

    search.connect("search-changed", refilter)
    cancel.connect("clicked", lambda *_: finish(None))
    ok.connect("clicked", done)
    win.connect("close-request", lambda *_: (finish(None), False)[1])
    keys = Gtk.EventControllerKey()
    keys.connect("key-pressed", key)
    win.add_controller(keys)
    GLib.timeout_add_seconds(POPUP_TIMEOUT, lambda: (finish("timeout"), False)[1])
    open_tab(cur["i"])  # --start on the other side: that tab is wanted, load it now
    if profile and profile.get("selected") is not None:
        preset_buttons[profile["selected"]].set_active(True)  # the preset named on the command line
    render()
    win.present()
    loop.run()
    return result["value"]


# ---------------------------------------------------------------- output

def write_outputs(out_dir, docs, texts, paths):
    """Write one .txt per document; only files this tool wrote earlier are removed first."""
    os.makedirs(out_dir, exist_ok=True)
    manifest = os.path.join(out_dir, ".manifest.json")
    try:
        with open(manifest, encoding="utf-8") as f:
            old = json.load(f)
    except (OSError, ValueError):
        old = []
    for fn in old:
        if os.path.basename(fn) == fn:  # never follow a path out of the folder
            try:
                os.remove(os.path.join(out_dir, fn))
            except OSError:
                pass
    written = []
    for d in docs:
        fn = d.replace("/", " - ") + ".txt"
        with open(os.path.join(out_dir, fn), "w", encoding="utf-8") as f:
            f.write(f"SOURCE: {paths[d]}\n\n{texts[d]}\n")
        written.append(fn)
    with open(manifest, "w", encoding="utf-8") as f:
        json.dump(written, f)
    return [os.path.join(out_dir, fn) for fn in written]


# ---------------------------------------------------------------- usage log (local, anonymous)
# One line per load in ~/.local/share/researcher/usage.jsonl: counts and character sums only.
# Never a title, path, set name or library location. The user can look at the file, print
# totals (--usage) and write an export (--usage-export). Records are only sent when the user
# said yes (`[usage] share = true`, set with --usage-share on): --usage-send sends the ones not
# sent yet, and after a load that happens in the background.

EXPORT_SCHEMA = 1  # the file --usage-export writes; each record inside has its own version
# Record versions: 1 had counts per format (pdf, html, md, txt); 2 adds chats loaded as context to
# `fmt` and `fmt_chars`, the loaded characters per format. Both are read and sent.
RECORD_VERSION = 2
# Reading the converted text instead of the original costs fewer tokens: measured with count_tokens
# on 85 reports (README, "What it saves"), raw HTML = 1.73x its text and a PDF read as a document
# = 2.2x. So per converted character HTML saves 0.73 and PDF 1.2 (markdown, text and chats are read
# as they are). Keep these equal to SAVED_PER_CHAR in worker/src/index.js.
SAVED_PER_CHAR = {"html": 0.73, "pdf": 1.2}
SEND_SCHEMA = 2  # the request --usage-send posts: the records plus `seq`, their log line number
DEFAULT_URL = "https://the-researcher-usage.ludalunizz.workers.dev/v1/usage"  # worker/ in the repo
SEND_BATCH = 200  # records per request (the receiver takes up to 200)
SEND_TIMEOUT = 15  # seconds
USAGE_KINDS = ("pdf", "html", "md", "txt")
FORMS = USAGE_KINDS + ("chat",)  # what a record counts per format


def data_home():
    base = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
    return os.path.join(base, APP)


def usage_file():
    return os.path.join(data_home(), "usage.jsonl")


def sent_file():
    return os.path.join(data_home(), "usage-sent")


def write_atomic(path, text):
    path = os.path.realpath(path)  # a symlinked file (dotfiles) stays a link: write where it points
    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "w", encoding="utf-8", newline="") as f:  # line endings exactly as given
        f.write(text)
    with contextlib.suppress(OSError):
        os.chmod(tmp, stat.S_IMODE(os.stat(path).st_mode))  # and keeps its permissions
    os.replace(tmp, path)


def usage_section(cfg):
    sec = cfg.get("usage")
    return sec if isinstance(sec, dict) else {}


def usage_enabled(cfg):
    return usage_section(cfg).get("log") is not False


def usage_share(cfg):
    """The user's answer to "share the anonymous usage counts?": "on", "off" or "not decided"."""
    val = usage_section(cfg).get("share")
    return "on" if val is True else "off" if val is False else "not decided"


def usage_url(cfg):
    """Where records are sent: `[usage] url`, or the project's receiver."""
    val = usage_section(cfg).get("url")
    return val if isinstance(val, str) and val else DEFAULT_URL


def install_id():
    """A random id for this install, made on first use; delete the file to start a new one."""
    path = os.path.join(data_home(), "usage-id")
    try:
        with open(path, encoding="utf-8") as f:
            val = f.read().strip()
        if re.fullmatch(r"[0-9a-f]{16}", val):
            return val
    except OSError:
        pass
    val = uuid.uuid4().hex[:16]
    os.makedirs(data_home(), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(val + "\n")
    return val


def format_of(path):
    ext = os.path.splitext(path)[1].lower().lstrip(".")
    return "html" if ext == "htm" else ext if ext in USAGE_KINDS else "txt"


def known_version(rec):
    return isinstance(rec, dict) and rec.get("v") in (1, 2)


def usage_build(counts, src_bytes, raw, lib):
    """One load as numbers. counts = {format: (documents, characters)}; lib = (documents, characters)
    of everything that could have been loaded. Characters, not tokens: the reader applies its ratio."""
    fmt = {k: counts.get(k, (0, 0))[0] for k in FORMS}
    fmt_chars = {k: counts.get(k, (0, 0))[1] for k in FORMS}
    return {"v": RECORD_VERSION, "day": datetime.date.today().isoformat(), "id": install_id(),
            "docs": sum(fmt.values()), "fmt": fmt, "fmt_chars": fmt_chars,
            "src_bytes": src_bytes, "raw": raw, "chars": sum(fmt_chars.values()),
            "lib_docs": lib[0], "lib_chars": lib[1], "tk": CHARS_PER_K}


def usage_record(docs, texts, paths):
    """The record of one library load."""
    counts, src_bytes = {}, 0
    for d in docs:
        n, c = counts.get(format_of(paths[d]), (0, 0))
        counts[format_of(paths[d])] = (n + 1, c + len(texts[d]))
        src_bytes += os.path.getsize(paths[d])
    lib = [d for d in paths if d in texts]
    return usage_build(counts, src_bytes, sum(texts.raw_chars(d) for d in docs),
                       (len(lib), sum(len(texts[d]) for d in lib)))


def file_size(path):
    try:
        return os.path.getsize(path)
    except OSError:
        return 0  # a transcript that vanished must not break a load


def chat_usage_record(loaded, lib):
    """The record of earlier Claude Code chats loaded as context: loaded = [(characters, transcript
    bytes)] of each chat loaded; lib = (chats, characters) of every chat up to the newest one picked. A chat
    is read as it is written, so nothing is squeezed (raw = chars)."""
    chars = sum(c for c, _ in loaded)
    return usage_build({"chat": (len(loaded), chars)}, sum(b for _, b in loaded), chars, lib)


def finish_usage(cfg, cfg_path, rec):
    """Log the record of a finished load and, when the user said yes, send it in the background.
    Returns the one line to print when sharing is not decided yet. Never breaks a load."""
    log_usage(rec)
    share = usage_share(cfg)
    if share == "on":
        send_in_background(cfg_path)
    elif share == "not decided":
        return "USAGE SHARING: not decided (researcher --usage-share on|off)"
    return None


def usage_of_chats(cfg, cfg_path, loaded, lib):
    """Log a chat load (and send it when sharing is on). Returns the sharing note, if any."""
    if not loaded or not usage_enabled(cfg):
        return None
    try:
        return finish_usage(cfg, cfg_path, chat_usage_record(loaded, lib))
    except OSError:
        return None


def log_usage(rec):
    """Append one record. A failure here must never break a load."""
    try:
        os.makedirs(data_home(), exist_ok=True)
        with open(usage_file(), "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, sort_keys=True) + "\n")
    except OSError:
        pass


def read_usage():
    """Records from the log; lines that are not this schema are skipped."""
    out = []
    try:
        with open(usage_file(), encoding="utf-8") as f:
            for line in f:
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                if known_version(rec):
                    out.append(rec)
    except OSError:
        pass
    return out


def usage_summary(recs):
    """Totals over records. Characters are summed; tokens use each record's own ratio."""
    def num(d, k):  # a damaged line counts as 0, as in the Rust version
        v = d.get(k)
        return v if isinstance(v, (int, float)) and not isinstance(v, bool) else 0

    def tok(chars, rec):
        return chars / (num(rec, "tk") or CHARS_PER_K) * 1000

    tot = {k: sum(num(r, k) for r in recs) for k in ("docs", "src_bytes", "raw", "chars")}
    tot["squeezed"] = tot["raw"] - tot["chars"]
    tot["unticked"] = sum(max(0, num(r, "lib_chars") - num(r, "chars")) for r in recs)
    tot["tokens"] = sum(tok(num(r, "chars"), r) for r in recs)
    tot["tokens_squeezed"] = sum(tok(num(r, "raw") - num(r, "chars"), r) for r in recs)
    tot["tokens_unticked"] = sum(tok(max(0, num(r, "lib_chars") - num(r, "chars")), r)
                                 for r in recs)
    tot["loads"] = len(recs)
    # reading text instead of the HTML/PDF originals (records that say their formats only)
    with_formats = [r for r in recs if isinstance(r.get("fmt_chars"), dict)]
    tot["loads_with_formats"] = len(with_formats)
    tot["tokens_converted"] = sum(
        tok(sum(per * num(r["fmt_chars"], k) for k, per in SAVED_PER_CHAR.items()), r)
        for r in with_formats)
    return tot


def usage_report(recs):
    if not recs:
        return "no usage recorded yet (a log line is written after every load)"
    t = usage_summary(recs)
    n = t["loads"]
    days = sorted({r.get("day", "") for r in recs})
    pct = 100 * t["squeezed"] // t["raw"] if t["raw"] else 0
    return "\n".join([
        f"usage log: {usage_file()}",
        f"{n} loads on {len(days)} days ({days[0]} to {days[-1]}), {t['docs']} documents loaded",
        f"loaded:        {t['chars']:>10} chars  (~{t['tokens'] / 1000:.0f}k tokens, "
        f"{t['tokens'] / n / 1000:.1f}k per load)",
        f"squeezed away: {t['squeezed']:>10} chars  (~{t['tokens_squeezed'] / 1000:.0f}k tokens, "
        f"{pct}% of the converted text)",
        f"converted:     ~{t['tokens_converted'] / 1000:.0f}k tokens saved by reading text instead of "
        f"the HTML/PDF originals (from measured ratios, {t['loads_with_formats']} of {n} loads say "
        "their formats)",
        f"left unticked: {t['unticked']:>10} chars  (~{t['tokens_unticked'] / 1000:.0f}k tokens, "
        "against loading the whole library every time: a ceiling, not a measurement)",
        "tokens are estimates from characters, not counted by a tokenizer",
    ])


def usage_pending():
    """The records not sent yet, oldest first, each with `seq` = its line number in the log. The
    line number tells two identical loads on one day apart, and it stays the same when a record
    is sent again, so a resend is stored once."""
    try:
        with open(usage_file(), encoding="utf-8") as f:
            lines = f.read().split("\n")
    except OSError:
        return []
    if lines and lines[-1] == "":
        lines.pop()  # the newline after the last record
    try:
        with open(sent_file(), encoding="utf-8") as f:
            done = int(f.read().strip())
    except (OSError, ValueError):
        done = 0
    if not 0 <= done <= len(lines):
        done = 0  # the log was deleted or cut: start again
    out = []
    for i, line in enumerate(lines[done:], done + 1):
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        if known_version(rec):
            rec["seq"] = i
            out.append(rec)
    return out


def usage_payload(recs):
    """The body --usage-send posts for these records."""
    return {"schema": SEND_SCHEMA, "loads": recs}


def post_json(url, body):
    """One POST: (HTTP status, body text). OSError when there was no answer."""
    req = urllib.request.Request(url, data=json.dumps(body, sort_keys=True).encode("utf-8"),
                                 headers={"Content-Type": "application/json",
                                          "User-Agent": "the-researcher (python)"},
                                 method="POST")
    try:
        with urllib.request.urlopen(req, timeout=SEND_TIMEOUT) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        try:
            return e.code, e.read().decode("utf-8", "replace")
        finally:
            e.close()
    except (urllib.error.URLError, ValueError) as e:
        raise OSError(str(getattr(e, "reason", e)))


def usage_send(url, recs):
    """Send in batches -> (done, error or None); done = {"sent", "new", "refused"}. After each
    batch the sent marker moves on, so a later failure never sends the earlier ones twice (and
    if it did, the receiver stores them once). A record the receiver refuses is skipped, so one
    bad line can never block the rest."""
    done = {"sent": 0, "new": 0, "refused": 0}
    for start in range(0, len(recs), SEND_BATCH):
        batch = recs[start:start + SEND_BATCH]
        left = list(batch)
        while left:
            try:
                status, text = post_json(url, usage_payload(left))
            except OSError as e:
                return done, str(e)
            try:
                reply = json.loads(text)
            except ValueError:
                reply = None
            reply = reply if isinstance(reply, dict) else {}
            if status == 200 and reply.get("ok") is True:
                done["sent"] += len(left)
                new = reply.get("new")
                done["new"] += new if isinstance(new, int) and new >= 0 else 0
                break
            err = reply.get("error") if isinstance(reply.get("error"), str) else ""
            m = re.match(r"record (\d+):", err)
            if status == 400 and m and int(m.group(1)) < len(left):
                # the receiver names the one record it refused: skip it, send the rest
                del left[int(m.group(1))]
                done["refused"] += 1
                continue
            return done, f"HTTP {status}: {err or text.strip()[:200]}"
        try:
            os.makedirs(data_home(), exist_ok=True)
            write_atomic(sent_file(), f"{batch[-1]['seq']}\n")
        except OSError:
            pass
    return done, None


def send_in_background(cfg_path):
    """Start --usage-send on its own and return at once: a load never waits on the network,
    and the sender outlives the load (its own session on POSIX)."""
    kw = {"start_new_session": True} if os.name == "posix" else {}
    try:
        subprocess.Popen([sys.executable, "-I", os.path.abspath(__file__), "--usage-send",
                          "--config", os.path.abspath(cfg_path)], stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **kw)
    except OSError:
        pass


def share_line(share, waiting):
    """The last line of --usage: is anything sent, and how much is waiting."""
    if share == "on":
        return (f"sharing: on, {waiting} records waiting to be sent "
                "(researcher --usage-send sends them now)")
    return f"sharing: {share}, nothing is sent (researcher --usage-share on|off)"


def set_share(cfg_path, on):
    """Put `share = true|false` under [usage] in the config (comments and the rest kept)."""
    line = f"share = {'true' if on else 'false'}"
    try:
        with open(cfg_path, encoding="utf-8", newline="") as f:  # keep the file's line endings
            old = f.read()
    except FileNotFoundError:
        old = None
    if old is None:
        body = f"[usage]\n{line}\n"
    else:
        h = re.search(r"(?m)^[ \t]*\[usage\][ \t]*(#[^\r\n]*)?\r?$", old)
        if not h and not old.strip():
            body = f"[usage]\n{line}\n"
        elif not h:  # no [usage] table yet: a new one at the end, after a blank line
            body = (old if old.endswith("\n") else old + "\n") + f"\n[usage]\n{line}\n"
        else:
            nl = old.find("\n", h.end())
            start = len(old) if nl < 0 else nl + 1
            nxt = re.search(r"(?m)^[ \t]*\[", old[start:])
            end = start + nxt.start() if nxt else len(old)
            section = old[start:end]
            key = re.compile(r"(?m)^[ \t]*share[ \t]*=.*$")
            if key.search(section):
                section = key.sub(lambda _: line, section, count=1)
            else:
                section = f"{line}\n{section}"
            head = old[:start] if old[:start].endswith("\n") else old[:start] + "\n"
            body = head + section + old[end:]
    if tomllib is None:
        raise ConfigError("Python 3.11 or newer is needed to read config.toml")
    try:
        parsed = tomllib.loads(body)
    except tomllib.TOMLDecodeError as e:
        raise ConfigError(str(e))
    if usage_share(parsed) != ("on" if on else "off"):
        raise ConfigError("the config would not read back the same answer")
    os.makedirs(os.path.dirname(os.path.abspath(cfg_path)), exist_ok=True)
    write_atomic(cfg_path, body)


def usage_send_main(cfg, dry_run):
    """--usage-send: send the records not sent yet. Exit 9 when sharing is not on."""
    share, url, recs = usage_share(cfg), usage_url(cfg), usage_pending()
    if dry_run:
        print(f"DRY RUN: {len(recs)} records would be sent to {url} (sharing: {share}); "
              "nothing was sent")
        if recs:
            print(json.dumps(usage_payload(recs), indent=1, sort_keys=True))
        return 0
    if share != "on":
        print(f"USAGE SHARING: {share}; nothing sent (turn it on with: researcher --usage-share on)")
        return 9
    if not recs:
        print("NOTHING TO SEND: every usage record was sent already")
        return 0
    done, err = usage_send(url, recs)
    if err is None:
        refused = (f", {done['refused']} refused by the receiver and skipped"
                   if done["refused"] else "")
        print(f"SENT: {done['sent']} records to {url} ({done['new']} new there{refused})")
        return 0
    print(f"SEND FAILED: {err}; {done['sent']} records sent before that, "
          f"{len(recs) - done['sent'] - done['refused']} stay queued for the next try")
    return 1


def usage_export(path):
    """Write the records as one JSON file to look at."""
    recs = read_usage()
    with open(path, "w", encoding="utf-8") as f:
        json.dump({"schema": EXPORT_SCHEMA, "loads": recs}, f, indent=1, sort_keys=True)
        f.write("\n")
    return len(recs)


# ---------------------------------------------------------------- sessions (opt-in)
# Closed Claude Code sessions: a registry keyed by the FULL session id, so a name can never
# be separated from the id it belongs to. Names are taken from Claude Code's own title or the
# first prompt, and can be renamed (a manual name is never overwritten by a rescan).

SESSION_ID_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
# opencode session ids: `ses_` and letters/digits (case matters)
OC_ID_RE = re.compile(r"ses_[A-Za-z0-9]+")
NAME_MAX = 80
SESSION_LOAD_MAX = 200000  # chars; a longer session keeps its start and its end
SESSION_DEFAULTS = {"idle_minutes": 10, "projects_dir": "~/.claude/projects", "extra_dirs": [], "opencode": False,
                    "opencode_cmd": "opencode"}


def sessions_settings(cfg):
    sec = cfg.get("sessions")
    if not isinstance(sec, dict) or not sec.get("enabled"):
        return None
    out = dict(SESSION_DEFAULTS)
    for k in out:
        if k in sec:
            out[k] = sec[k]
    out["projects_dir"] = os.path.abspath(os.path.expanduser(str(out["projects_dir"])))
    extra = out["extra_dirs"]
    extra = [extra] if isinstance(extra, str) else extra if isinstance(extra, list) else []
    out["extra_dirs"] = [os.path.abspath(os.path.expanduser(x.strip()))
                         for x in extra if isinstance(x, str) and x.strip()]
    out["opencode"] = bool(out["opencode"])
    idle = out["idle_minutes"]
    try:  # like the Rust version: a number or a numeric string, else the default
        out["idle_minutes"] = 10.0 if isinstance(idle, bool) else float(idle.strip() if isinstance(idle, str) else idle)
    except (TypeError, ValueError):
        out["idle_minutes"] = 10.0
    cmd = out["opencode_cmd"]
    out["opencode_cmd"] = os.path.expanduser(cmd.strip()) if isinstance(cmd, str) and cmd.strip() else "opencode"
    return out


def clean_name(text):
    text = re.sub(r"\s+", " ", str(text)).strip()
    return text if len(text) <= NAME_MAX else text[:NAME_MAX - 1].rstrip() + "…"


def block_text(content):
    """The plain text of a message: a string, or the text blocks of a list (tools skipped)."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(b.get("text", "") for b in content
                         if isinstance(b, dict) and b.get("type") == "text")
    return ""


def read_transcript(path):
    """Entries of a session .jsonl; unreadable or broken lines are skipped."""
    out = []
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                try:
                    d = json.loads(line)
                except ValueError:
                    continue
                if isinstance(d, dict):
                    out.append(d)
    except OSError:
        pass
    return out


def real_prompt(entry):
    """The user's own prompt of an entry, or '' (tool results, meta and system text, commands)."""
    if entry.get("type") != "user" or any(entry.get(k) for k in ("isMeta", "isCompactSummary", "isVisibleInTranscriptOnly")):
        return ""  # a compaction summary is Claude Code's text, not the user's
    msg = entry.get("message")
    # a large paste is the user's own prompt, wrapped in <pasted_content id="..."> tags
    text = re.sub(r"</?pasted_content[^>]*>", "", block_text(msg.get("content") if isinstance(msg, dict) else "")).strip()
    return "" if not text or text.startswith("<") else text


def session_info(entries):
    title = first = cwd = None
    started = last = None
    for e in entries:
        if e.get("type") == "custom-title" and e.get("customTitle"):
            title = e["customTitle"]
        elif e.get("type") == "ai-title" and e.get("aiTitle") and not title:
            title = e["aiTitle"]
        if first is None:
            first = real_prompt(e) or None
        cwd = cwd or e.get("cwd")
        ts = e.get("timestamp")
        if isinstance(ts, str):
            started = started or ts
            last = ts
    return {"title": title, "first": first, "cwd": cwd, "started": started, "last": last}


def load_registry(path):
    reg = load_conf(path)
    if isinstance(reg.get("sessions"), dict):
        return reg
    if reg:
        set_aside(path)
    return {"version": 1, "sessions": {}}


def save_registry(path, reg):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    write_atomic(path, json.dumps(reg, indent=1, ensure_ascii=False))


def register(reg, sid, path, mtime, proj, entries, source=None):
    """Add or refresh the registry record of one session from its transcript. Returns 1 when the
    session is new, 0 when it was known or nothing was ever asked."""
    import time
    info = session_info(entries)
    if not info["first"] and not info["title"]:
        return 0  # nothing was ever asked
    if source == "opencode" and info["title"]:
        # a chat started from a command or skill opens with its whole expanded text: opencode's
        # own title is the better label
        info["first"] = info["title"]
    auto = clean_name(info["title"] or info["first"])
    rec = reg["sessions"].get(sid)
    added = 0
    if rec is None:
        rec = reg["sessions"][sid] = {"name": auto, "name_source": "auto",
                                      "registered": time.strftime("%Y-%m-%d %H:%M")}
        added = 1
    elif rec.get("name_source") != "manual":
        rec["name"] = auto
    rec.update(file=path, mtime=mtime, project=proj, cwd=info["cwd"],
               started=info["started"], last=info["last"],
               first=clean_name(info["first"]) if info["first"] else None)
    if source:
        rec["source"] = source
    rec["chars"] = len(session_text(sid, rec, entries))
    return added


def up_to_date(reg, sid, path, mtime):
    """True when the registry already holds this version of the session's transcript."""
    rec = reg["sessions"].get(sid)
    return bool(rec and rec.get("mtime") == mtime and rec.get("file") == path
                and "first" in rec and "chars" in rec)


def project_bases(settings):
    """The folders that hold project folders: projects_dir, then each of extra_dirs itself and
    every <extra>/<name>/projects in it (the layout of chats moved away on /clear)."""
    out = [settings["projects_dir"]]
    for x in settings.get("extra_dirs", []):
        out.append(x)
        try:
            subs = sorted(os.listdir(x), reverse=True)  # newest dated folder first
        except OSError:
            continue
        out += [os.path.join(x, d, "projects") for d in subs if os.path.isdir(os.path.join(x, d, "projects"))]
    return out


def scan_sessions(reg, settings, now=None, idle=None, skip="", reg_path="", in_opencode=False, live_out=None):
    """Add sessions untouched for `idle` seconds (default: idle_minutes) to the registry and
    refresh the changed ones (never `skip`, the session that runs this). Returns the number added.
    opencode chats are added too when `[sessions] opencode` is on; `in_opencode` = this runs inside
    opencode (its newest chat is the one that runs this)."""
    import time
    now = time.time() if now is None else now
    idle = float(settings["idle_minutes"]) * 60 if idle is None else idle
    added = 0
    seen = set()
    for base in project_bases(settings):
        try:
            projects = sorted(os.listdir(base))
        except OSError:
            continue
        for proj in projects:
            pdir = os.path.join(base, proj)
            try:
                files = sorted(os.listdir(pdir))
            except OSError:
                continue
            for fn in files:
                sid = fn[:-6]
                if not fn.endswith(".jsonl") or not SESSION_ID_RE.fullmatch(sid) or sid == skip or sid in seen:
                    continue  # the first copy wins: projects_dir before extra_dirs
                seen.add(sid)
                path = os.path.join(pdir, fn)
                try:
                    mtime = os.path.getmtime(path)
                except OSError:
                    continue
                if now - mtime < idle:
                    continue  # probably still open
                if up_to_date(reg, sid, path, mtime):
                    continue
                added += register(reg, sid, path, mtime, proj, read_transcript(path))
    if settings["opencode"]:
        added += scan_opencode(reg, settings, now, idle, reg_path, in_opencode, live_out)
    return added


def oc_run(cmd, args):
    """Run the opencode program and return its standard output."""
    try:
        out = subprocess.run([cmd, *args], stdin=subprocess.DEVNULL, capture_output=True, check=False)
    except OSError as e:
        raise ConfigError(f"{cmd}: {e.strerror or e}") from e
    if out.returncode != 0:
        raise ConfigError(f"{cmd} {' '.join(args)} failed")
    return out.stdout


def iso_ms(ms):
    t = datetime.datetime.fromtimestamp(ms // 1000, datetime.timezone.utc)
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{ms % 1000:03d}Z"


def oc_text(parts):
    """The text parts of an opencode message (tool calls, reasoning, synthetic and ignored text left out)."""
    texts = [p.get("text", "") for p in parts or []
             if isinstance(p, dict) and p.get("type") == "text"
             and not p.get("synthetic") and not p.get("ignored") and isinstance(p.get("text"), str)]
    return "\n".join(texts).strip()


def oc_entries(export, directory):
    """`opencode export` as transcript entries in the shape of a Claude Code .jsonl (title, user and
    assistant text with timestamps), so the rest of the program reads both the same way."""
    out = []
    info = export.get("info") if isinstance(export, dict) else None
    title = info.get("title") if isinstance(info, dict) else ""
    if isinstance(title, str) and title and not title.startswith("New session - ") and not title.startswith("<"):
        out.append({"type": "ai-title", "aiTitle": title})
    for m in (export.get("messages") if isinstance(export, dict) else None) or []:
        mi = m.get("info") if isinstance(m, dict) else None
        role = mi.get("role") if isinstance(mi, dict) else ""
        text = oc_text(m.get("parts"))
        if not text or role not in ("user", "assistant"):
            continue
        created = (mi.get("time") or {}).get("created") if isinstance(mi.get("time"), dict) else 0
        content = text if role == "user" else [{"type": "text", "text": text}]
        out.append({"type": role, "timestamp": iso_ms(int(created or 0)), "cwd": directory,
                    "message": {"role": role, "content": content}})
    return out


def scan_opencode(reg, settings, now, idle, reg_path, in_opencode, live_out=None):
    """Add opencode's chats (top-level ones; sub-agent sessions are not listed by opencode) to the
    registry. Each changed chat is exported once and cached as a transcript next to the registry."""
    from concurrent.futures import ThreadPoolExecutor
    cmd = settings["opencode_cmd"]
    try:
        listing = json.loads(oc_run(cmd, ["session", "list", "--format", "json"]))
        if not isinstance(listing, list):
            raise ConfigError("unreadable session list")
    except (ConfigError, ValueError) as e:
        print(f"OPENCODE SKIPPED ({e}): opencode chats left out")
        return 0

    def upd(v):
        u = v.get("updated") if isinstance(v, dict) else 0
        return u / 1000.0 if isinstance(u, (int, float)) else 0.0
    live = None
    if in_opencode:  # the chat that runs this is the newest one, still being written
        recent = [v for v in listing if isinstance(v, dict) and now - upd(v) < 600]
        if recent:
            live = max(recent, key=upd).get("id")
    if live_out is not None and live:
        live_out.append(live)  # an earlier scan from Claude Code may have registered it
    cache = os.path.join(os.path.dirname(reg_path), "sessions-opencode")
    todo, added = [], 0
    for v in listing:
        sid = v.get("id") if isinstance(v, dict) else None
        if not isinstance(sid, str) or not OC_ID_RE.fullmatch(sid):
            continue
        mtime = upd(v)
        if sid == live or now - mtime < idle:
            continue
        path = os.path.join(cache, f"{sid}.jsonl")
        if up_to_date(reg, sid, path, mtime) and os.path.isfile(path):
            continue
        try:
            fresh = os.path.getmtime(path) >= mtime
        except OSError:
            fresh = False
        if fresh:  # exported after its last change (an empty chat never gets a record): read the copy
            added += register(reg, sid, path, mtime, "opencode", read_transcript(path), source="opencode")
            continue
        todo.append((sid, mtime, v.get("directory") or "", path))

    def export(item):
        try:
            return json.loads(oc_run(cmd, ["export", item[0]]))
        except (ConfigError, ValueError):
            return None
    # opencode takes about two seconds per export: four at a time
    with ThreadPoolExecutor(max_workers=4) as pool:
        exports = list(pool.map(export, todo))
    for (sid, mtime, directory, path), ex in zip(todo, exports):
        if ex is None:
            continue
        entries = oc_entries(ex, directory)
        try:
            os.makedirs(cache, exist_ok=True)
            tmp = f"{path}.{os.getpid()}.tmp"
            with open(tmp, "w", encoding="utf-8") as f:
                f.write("".join(json.dumps(e, ensure_ascii=False) + "\n" for e in entries))
            os.replace(tmp, path)
        except OSError:
            continue
        added += register(reg, sid, path, mtime, "opencode", entries, source="opencode")
    return added


def find_session(reg, ident):
    """A registry id from a full id or a unique prefix (4+ characters)."""
    ident = ident.strip().lower()
    # opencode ids mix upper and lower case: match without case, answer with the real id
    exact = [i for i in reg["sessions"] if i.lower() == ident]
    if exact:
        return exact[0]
    hits = [i for i in reg["sessions"] if len(ident) >= 4 and i.lower().startswith(ident)]
    if len(hits) == 1:
        return hits[0]
    raise ConfigError(f"no session matches '{ident}'" if not hits else
                      f"'{ident}' matches {len(hits)} sessions; give more of the id")


def session_label(sid, rec):
    """Name and full id, always together."""
    return f'"{rec["name"]}" ({sid})'


def session_line(sid, rec):
    when = (rec.get("last") or rec.get("started") or "")[:16].replace("T", " ")
    return f"{sid}  {when:16}  {rec['name']}"


def resume_command(sid, rec):
    cwd = rec.get("cwd")
    cmd = f"opencode -s {sid}" if rec.get("source") == "opencode" else f"claude --resume {sid}"
    return f"cd {shlex.quote(cwd)} && {cmd}" if cwd else cmd


def session_text(sid, rec, entries=None):
    """The conversation of a session as plain text: prompts and replies, no tool traffic."""
    parts = [f"SESSION: {rec['name']}", f"SESSION ID: {sid}",
             f"PROJECT FOLDER: {rec.get('cwd') or rec.get('project', '')}",
             f"LAST ACTIVE: {(rec.get('last') or '')[:16].replace('T', ' ')}", ""]
    for e in read_transcript(rec["file"]) if entries is None else entries:
        if e.get("type") == "user":
            text = real_prompt(e)
            who = "USER"
        elif e.get("type") == "assistant":
            msg = e.get("message")
            text = block_text(msg.get("content") if isinstance(msg, dict) else "").strip()
            who = "ASSISTANT"
        else:
            continue
        if text:
            parts.append(f"{who}: {text}\n")
    body = "\n".join(parts)
    if len(body) > SESSION_LOAD_MAX:
        keep = SESSION_LOAD_MAX // 5
        body = (body[:keep] + f"\n\n[... {len(body) - SESSION_LOAD_MAX} characters left out "
                "of the middle ...]\n\n" + body[-(SESSION_LOAD_MAX - keep):])
    return body


def sessions_main(args, cfg, cfg_path, reg_path, out_dir):
    settings = sessions_settings(cfg)
    if settings is None:
        print("SESSIONS OFF: add `[sessions]` with `enabled = true` to the config to track "
              "closed sessions; nothing done")
        return 7
    reg = load_registry(reg_path)
    added = scan_sessions(reg, settings, reg_path=reg_path, in_opencode="OPENCODE" in os.environ)
    try:
        if args.rename:
            sid = find_session(reg, args.rename[0])
            set_name(reg, sid, args.rename[1])  # an empty name gives it its automatic name back
            save_registry(reg_path, reg)
            print(f"RENAMED {session_label(sid, reg['sessions'][sid])}")
            return 0
        with contextlib.suppress(OSError):
            save_registry(reg_path, reg)
        if args.resume:
            sid = find_session(reg, args.resume)
            print(f"SESSION {session_label(sid, reg['sessions'][sid])}")
            print(resume_command(sid, reg["sessions"][sid]))
            return 0
        if args.session_load:
            sid = find_session(reg, args.session_load)
            rec = reg["sessions"][sid]
            if not os.path.isfile(rec.get("file", "")):
                raise ConfigError(f"the transcript of {session_label(sid, rec)} is gone")
            text = session_text(sid, rec)
            doc = f"sessions/{sid}"
            written = write_outputs(out_dir, [doc], {doc: text}, {doc: rec["file"]})
            print(f"SESSION {session_label(sid, rec)}")
            print(f"{len(text):>7} chars  {written[0]}")
            print(f"total: 1 session, {len(text)} chars (~{len(text) // CHARS_PER_K}k tokens)")
            # one chat, against every chat up to it (last active at or before it)
            note = usage_of_chats(cfg, cfg_path, [(len(text), file_size(rec["file"]))],
                                  chats_up_to((reg["sessions"][s] for s in chat_catalog(reg)), last_of(rec)))
            if note:
                print(note)
            return 0
    except (ConfigError, OSError) as e:
        print(f"SESSION ERROR: {e}")
        return 8
    # --context also registers sessions that were open a moment ago; the list shows closed ones
    import time
    now, idle = time.time(), float(settings["idle_minutes"]) * 60
    items = sorted(((sid, rec) for sid, rec in reg["sessions"].items()
                    if not isinstance(rec.get("mtime"), (int, float)) or now - rec["mtime"] >= idle),
                   key=lambda kv: kv[1].get("last") or kv[1].get("started") or "", reverse=True)
    print(f"{len(items)} closed sessions tracked ({added} new); newest first")
    for sid, rec in items[:args.limit]:
        print(session_line(sid, rec))
    if len(items) > args.limit:
        print(f"... {len(items) - args.limit} more (--limit N)")
    return 0


# ---------------------------------------------------------------- --context: earlier chats

PRESET_RE = re.compile(r"[a-z0-9][a-z0-9._-]*")
DEFAULT_PRESET = "default"


def shown_name(rec):
    """What a chat is called in the picker: a name the user gave it, otherwise its first prompt."""
    if rec.get("first") and rec.get("name_source") != "manual":
        return rec["first"]
    return rec.get("name", "")


def chat_folder(rec):
    """The heading a chat sits under: its project folder (~ for the home folder)."""
    cwd = rec.get("cwd")
    if cwd:
        home = os.path.expanduser("~")
        if cwd == home or cwd.startswith(home + os.sep):
            return "~" + cwd[len(home):]
        return cwd
    return rec.get("project", "")


def last_of(rec):
    return rec.get("last") or rec.get("started") or ""


def chats_up_to(recs, cutoff):
    """(chats, characters) of the chats last active at or before `cutoff` (all of them when the
    cutoff is unknown): what loading everything up to that chat would have cost."""
    n = c = 0
    for r in recs:
        if not cutoff or last_of(r) <= cutoff:
            n += 1
            c += r.get("chars") or 0
    return n, c


def chat_catalog(reg, skip=()):
    """Every chat that can be loaded, by folder (the folder used most recently first), newest
    first inside a folder."""
    folders = {}
    for sid, rec in reg["sessions"].items():
        if sid in skip or not os.path.isfile(rec.get("file") or ""):
            continue
        folders.setdefault(chat_folder(rec), []).append((last_of(rec), sid))
    # sorts are stable (also with reverse=True): newest first, ties by name
    order = sorted(sorted(folders), key=lambda f: max(t for t, _ in folders[f]), reverse=True)
    out = []
    for f in order:
        inner = sorted(sorted(folders[f], key=lambda x: x[1]), key=lambda x: x[0], reverse=True)
        out += [sid for _, sid in inner]
    return out


def chat_sections(names, catalog, presets, setup):
    """One popup section per preset, like a custom set; Remember is only pre-ticked on first use
    (new chats come all the time; a one-off load must not replace the preset)."""
    out = []
    for n in names:
        saved = presets.get(n)
        selected = set(saved.get("selected", [])) if saved else set()
        known = set(saved.get("known", [])) if saved else set()
        main = [d for d in catalog if d in selected]
        out.append({"name": n, "kind": "custom", "docs": list(catalog), "main": main,
                    "other": [d for d in catalog if d not in selected],
                    "new": {d for d in catalog if d not in known} if saved else set(),
                    "ticked": set(main), "remember": saved is None or setup,
                    "first": saved is None})
    return out


def reset_name(reg, sid):
    """Give a chat its automatic name back (its title or first prompt, as a rescan would)."""
    rec = reg["sessions"].get(sid)
    if rec is None:
        return
    rec["name_source"] = "auto"
    entries = read_transcript(rec.get("file") or "")
    if not entries:  # the transcript is gone: nothing to read a title from, so the first prompt it had
        if clean_name(rec.get("first") or ""):
            rec["name"] = clean_name(rec["first"])
        return
    register(reg, sid, rec.get("file") or "", rec.get("mtime") or 0, rec.get("project") or "",
             entries, rec.get("source"))


def set_name(reg, sid, name):
    """Name a chat (an empty name gives it its automatic name back)."""
    name = clean_name(name)
    if not name:
        reset_name(reg, sid)
    elif sid in reg["sessions"]:
        reg["sessions"][sid].update(name=name, name_source="manual")


def save_presets(presets, picks, catalog, setup):
    """Put the remembered picks into the presets (a new name moves the ticks: the old name goes).
    Returns True when something changed."""
    saved = False
    for p in picks:
        if not (p["remember"] or setup):
            continue
        to = p["save_as"] or p["name"]
        if to != p["name"] and presets.pop(p["name"], None) is not None:
            print(f"RENAMED context preset {p['name']} to {to}")
        presets[to] = {"selected": [d for d in catalog if d in p["ticked"]], "known": list(catalog)}
        print(f"SAVED context preset {to}: {len(p['ticked'])} of {len(catalog)} chats")
        saved = True
    return saved


def context_prep(args, cfg, paths, names, secondary=False):
    """Find the chats for --context [PRESET ...] (or, as the second tab, the preset `default`).
    Returns a dict, or an int exit code (the message is printed, unless secondary)."""
    say = (lambda m: None) if secondary else print
    settings = sessions_settings(cfg)
    if settings is None:
        say("SESSIONS OFF: add `[sessions]` with `enabled = true` to the config to load earlier "
            "chats; nothing done")
        return 7
    presets = load_conf(paths["presets"])
    if args.list and not secondary:
        if not presets:
            print("no context presets saved yet")
        for n, rec in presets.items():
            print(f"{n:20} {len(set(rec.get('selected', []))):>4} chats")
        return 0
    names = list(dict.fromkeys(n.strip().lower() for n in names)) or [DEFAULT_PRESET]
    bad = [n for n in names if not PRESET_RE.fullmatch(n)]
    if bad:
        say(f"bad preset name(s): {', '.join(bad)} (use lowercase letters, digits, . _ -)")
        return 2

    me = os.environ.get("CLAUDE_CODE_SESSION_ID", "").lower()  # never offer the chat running this
    reg = load_registry(paths["reg"])
    live = []  # inside opencode: the opencode chat that runs this
    scan_sessions(reg, settings, idle=0, skip=me, reg_path=paths["reg"], in_opencode="OPENCODE" in os.environ,
                  live_out=live)
    try:
        save_registry(paths["reg"], reg)
    except OSError as e:
        say(f"could not save the session registry to {paths['reg']}: {e}")
    catalog = chat_catalog(reg, (me, *live))
    if not catalog:
        if settings["opencode"]:
            say(f"NO CHATS: no earlier Claude Code ({settings['projects_dir']}) or opencode chats; "
                "nothing loaded")
        else:
            say(f"NO CHATS: no earlier Claude Code chats in {settings['projects_dir']}; nothing loaded")
        return 0
    secs = chat_sections(names, catalog, presets, args.setup)
    if secondary:
        for s in secs:
            s["remember"] = False  # a second tab never saves a preset unless the user asks
    return {"presets": presets, "names": names, "reg": reg, "catalog": catalog, "secs": secs,
            "secondary": secondary}


def context_tab(c, ui_path):
    """The popup tab of the chats."""
    recs, catalog = c["reg"]["sessions"], c["catalog"]
    chip, by_time = ui_state(ui_path, True)
    times, notes = {}, {}
    for sid in catalog:
        w = parse_iso(last_of(recs[sid]))
        if w is None and isinstance(recs[sid].get("mtime"), (int, float)):
            w = int(recs[sid]["mtime"])
        if w is not None:
            times[sid] = w
            notes[sid] = local_time(w).strftime("%Y-%m-%d")  # the local day, like the headings
    look = {"chats": True, "chip": chip, "by_time": by_time, "taken": set(c["presets"]), "times": times,
            "labels": {sid: shown_name(recs[sid]) for sid in catalog},
            "defaults": {sid: recs[sid]["first"] for sid in catalog if recs[sid].get("first")},
            "groups": {sid: chat_folder(recs[sid]) for sid in catalog},
            "notes": notes}
    return {"sections": c["secs"], "sizes": {sid: int(recs[sid].get("chars") or 0) for sid in catalog},
            "look": look}


def context_finish(c, args, cfg, cfg_path, paths, out_dir, picks):
    """Save and load what was picked (picks None: no popup, the saved presets). Returns the exit
    code."""
    presets, names, reg, catalog, secs = c["presets"], c["names"], c["reg"], c["catalog"], c["secs"]
    unsaved = [n for n in names if n not in presets]
    if picks is None and unsaved:
        if c["secondary"]:
            return 0
        print(f"NO POPUP: context preset {', '.join(unsaved)} has no saved choice yet; nothing loaded")
        return 5
    # a saved preset can name a chat whose transcript was deleted since (the popup just drops it;
    # a profile preset passes it on)
    saved = ([d for n in names for d in presets[n].get("selected", [])] if picks is None
             else [d for p in picks["picks"] for d in p["ticked"]])
    have = set(catalog)
    # the running chat (and the live opencode one) is left out of the catalog, but not gone
    on_disk = lambda d: os.path.isfile((reg["sessions"].get(d) or {}).get("file") or "")
    gone = list(dict.fromkeys(d for d in saved if d not in have and not on_disk(d)))
    if picks is None:
        chosen_picks = [{"name": s["name"], "ticked": s["ticked"], "remember": False,
                         "save_as": s["name"], "title": None} for s in secs]
    else:
        try:
            save_ui(paths["ui"], True, picks["chip"], picks["by_time"])
        except OSError as e:
            print(f"could not save the window settings to {paths['ui']}: {e}")
        fresh = load_registry(paths["reg"]) if picks["renames"] else None
        for sid, name in picks["renames"].items():
            set_name(reg, sid, name)
            set_name(fresh, sid, name)  # renamed in the file as it is now: another run's rename stays
            print(f'RENAMED "{shown_name(reg["sessions"][sid])}" ({sid})')
        if picks["renames"]:
            try:
                save_registry(paths["reg"], fresh)
            except OSError as e:
                print(f"could not save the session registry to {paths['reg']}: {e}")
        chosen_picks = picks["picks"]
    if c["secondary"] and not any(p["ticked"] for p in chosen_picks):
        return 0  # the other tab, left alone

    if any(p["remember"] or args.setup for p in chosen_picks):
        presets = load_conf(paths["presets"])  # a preset another run saved meanwhile stays
    if save_presets(presets, chosen_picks, catalog, args.setup):
        try:
            save_conf(paths["presets"], presets)
        except OSError as e:
            print(f"could not save the presets to {paths['presets']}: {e}")
    if args.setup:
        print("setup only: nothing loaded")
        return 0

    recs = reg["sessions"]
    chosen = {d for p in chosen_picks for d in p["ticked"]}
    sids = [d for d in catalog if d in chosen]
    docs = [f"sessions/{sid}" for sid in sids]
    texts = {f"sessions/{sid}": session_text(sid, recs[sid]) for sid in sids}
    try:
        written = write_outputs(out_dir, docs, texts, {f"sessions/{sid}": recs[sid]["file"] for sid in sids})
    except OSError as e:
        print(f"SESSION ERROR: could not write to {out_dir}: {e}")
        return 8
    total = 0
    for sid, doc, path in zip(sids, docs, written):
        total += len(texts[doc])
        print(f'CHAT "{shown_name(recs[sid])}" ({sid})')
        print(f"{len(texts[doc]):>7} chars  {path}")
    for sid in gone:
        print(f"MISSING  chat {sid} (its transcript is gone)")
    print(f"total: {len(docs)} chats, {total} chars (~{total // CHARS_PER_K}k tokens)")
    # the "whole library" of this load: every chat up to the newest one picked
    newest = max((last_of(recs[sid]) for sid in sids), default="")
    note = usage_of_chats(cfg, cfg_path,
                          [(len(texts[doc]), file_size(recs[sid]["file"]))
                           for sid, doc in zip(sids, docs)],
                          chats_up_to((recs[sid] for sid in catalog), newest))
    if note:
        print(note)
    return 0



# ---------------------------------------------------------------- main

def main(argv=None):
    ap = argparse.ArgumentParser(prog="researcher.py", add_help=True,
                                 description="Load saved research documents into Claude Code.")
    ap.add_argument("sets", nargs="*", metavar="SET")
    ap.add_argument("--setup", action="store_true", help="change saved defaults, load nothing")
    ap.add_argument("--no-popup", action="store_true",
                    help="use the saved defaults, ask nothing (same as RESEARCHER_NO_POPUP=1)")
    ap.add_argument("--list", action="store_true", help="list sets and saved defaults")
    ap.add_argument("--sessions", action="store_true",
                    help="list tracked closed sessions (needs [sessions] enabled = true)")
    ap.add_argument("--rename", nargs=2, metavar=("ID", "NAME"), help="rename a tracked session")
    ap.add_argument("--resume", metavar="ID", help="print the command that resumes a session")
    ap.add_argument("--session-load", metavar="ID",
                    help="write a tracked session's conversation as a .txt to load as context")
    ap.add_argument("--context", action="store_true",
                    help="pick earlier Claude Code chats (all folders) to load as context; "
                         "SET names are presets")
    ap.add_argument("--start", choices=("code", "chats", "research"),
                    help="the tab the window opens on (default: chats with --context, code with --profile, "
                         "research otherwise)")
    ap.add_argument("--profile", metavar="NAME",
                    help="a project profile: code, chats and research in one window, with presets; "
                         "SET = the preset")
    ap.add_argument("--limit", type=int, default=30, help="sessions shown by --sessions (default 30)")
    ap.add_argument("--init", nargs="?", const="", metavar="LIBRARY", help="write a starter config")
    ap.add_argument("--config", help="config.toml path (default ~/.config/researcher/config.toml)")
    ap.add_argument("--library", help="library folder (overrides config and RESEARCHER_LIBRARY)")
    ap.add_argument("--sync", action="store_true",
                    help="make or refresh the plain-text copy of the whole library, load nothing")
    ap.add_argument("--usage", action="store_true",
                    help="print the local usage totals (characters and estimated tokens saved)")
    ap.add_argument("--usage-export", nargs="?", const="researcher-usage.json", metavar="FILE",
                    help="write the anonymous usage records to FILE (default researcher-usage.json) "
                         "to look at; nothing is sent")
    ap.add_argument("--usage-share", choices=("on", "off"),
                    help="share the anonymous usage counts with the project (on) or not (off); "
                         "saved in the config")
    ap.add_argument("--usage-send", action="store_true",
                    help="send the usage records not sent yet (only when sharing is on)")
    ap.add_argument("--dry-run", action="store_true",
                    help="with --usage-send: print what would be sent, send nothing")
    ap.add_argument("--out", help="the researcher's text folder (default ~/.local/share/researcher/text, "
                                  "or text_dir in the config); session loads go to ~/.cache/researcher")
    args = ap.parse_args(argv)
    if args.dry_run and not args.usage_send:
        ap.error("--dry-run goes with --usage-send")

    cfg_path = args.config or os.path.join(config_home(), "config.toml")
    state_path = os.path.join(os.path.dirname(cfg_path), "choices.json")

    if args.usage or args.usage_send or args.usage_share:
        try:
            cfg = load_config(cfg_path)
        except ConfigError as e:
            print(f"CONFIG ERROR: {e}")
            return 6
        if args.usage_share:
            on = args.usage_share == "on"
            try:
                set_share(cfg_path, on)
            except (ConfigError, OSError) as e:
                print(f"CONFIG ERROR: could not save `share` under [usage] in {cfg_path}: {e}")
                return 6
            if on:
                print(f"USAGE SHARING: on (saved as `share = true` under [usage] in {cfg_path}); "
                      f"the anonymous counts are sent to {usage_url(cfg)} after each load. "
                      "Undo: researcher --usage-share off")
            else:
                print(f"USAGE SHARING: off (saved as `share = false` under [usage] in {cfg_path}); "
                      "nothing is sent")
            return 0
        if args.usage_send:
            return usage_send_main(cfg, args.dry_run)
        print(usage_report(read_usage()))
        print(share_line(usage_share(cfg), len(usage_pending())))
        return 0
    if args.usage_export:
        try:
            n = usage_export(args.usage_export)
        except OSError as e:
            print(f"could not write {args.usage_export}: {e}")
            return 1
        print(f"wrote {n} records to {os.path.abspath(args.usage_export)}")
        print("each record: day, random install id, document counts per format, character sums "
              "(source bytes, converted, squeezed, whole library). No titles, paths or set names. "
              "Nothing was sent.")
        return 0

    if args.init is not None:
        lib = args.init or os.path.expanduser("~/research")
        return init_config(cfg_path, lib)

    if args.sessions or args.rename or args.resume or args.session_load:
        try:
            cfg = load_config(cfg_path)
        except ConfigError as e:
            print(f"CONFIG ERROR: {e}")
            return 6
        return sessions_main(args, cfg, cfg_path,
                             os.path.join(os.path.dirname(cfg_path), "sessions.json"),
                             args.out or cache_home())

    if args.profile:
        return load_profile(args, args.profile, cfg_path, state_path)
    return load_both(args, cfg_path, state_path)


def research_prep(args, cfg_path, state_path, secondary=False):
    """Find the research for `researcher SET ...` (or, as the second tab, the config's
    default_set). Returns a dict, or an int exit code (the message is printed, unless
    secondary)."""
    say = (lambda m: None) if secondary else print
    try:
        cfg = load_config(cfg_path)
        library = library_path(cfg, args.library)
        if not library:
            say(f"NO LIBRARY: set `library` in {cfg_path} (or run --init, or set "
                "RESEARCHER_LIBRARY); nothing loaded")
            return 6
        if not os.path.isdir(library):
            say(f"NO LIBRARY: {library} is not a folder; nothing loaded")
            return 6
        paths = scan(library, str_list(cfg.get("exclude"), "exclude"))
        text_dir = cfg_str(cfg, "text_dir")
        conf = load_conf(state_path)
        # the set names on the command line belong to the tab the command is for
        names = resolve_names([] if secondary else args.sets, cfg)
        sets, kinds = build_sets(cfg, list(paths), conf, names)
    except ConfigError as e:
        say(f"CONFIG ERROR: {e}")
        return 6

    # the second tab keeps its text copy where it always is; --out is for the first tab
    store = os.path.abspath(os.path.expanduser((None if secondary else args.out) or text_dir or text_home()))
    real_store, real_lib = os.path.realpath(store), os.path.realpath(library)
    if real_store == real_lib or real_store.startswith(real_lib.rstrip(os.sep) + os.sep):
        # the copies would overwrite the user's own files, or be read as documents again
        say(f"CONFIG ERROR: the text folder {store} is inside the library {library}; "
            "set text_dir (or --out) to a folder outside it")
        return 6

    if args.sync and not secondary:
        texts = Texts(paths, store)
        ok = [d for d in paths if d in texts]
        texts.flush()
        chars = sum(len(texts[d]) for d in ok)
        for d in paths:
            if d not in texts:
                print(f"UNREADABLE  {paths[d]} ({texts.errors[d]})")
        print(f"text copy: {len(ok)} of {len(paths)} docs in {store}, {chars} chars "
              f"(~{chars // CHARS_PER_K}k tokens)")
        return 0

    if args.list and not secondary:
        print(f"library: {library} ({len(paths)} documents)")
        for n, docs in sets.items():
            if n == "all" or kinds[n] != "custom" or n in conf:
                saved = conf[n].get("selected") if isinstance(conf.get(n), dict) else None
                sel = f", saved default {len(saved)}" if saved is not None else ""
                print(f"{n:20} {len(docs):>4} docs  ({kinds[n]}{sel})")
        if isinstance(cfg.get("aliases"), dict) and cfg["aliases"]:
            print("aliases: " + ", ".join(f"{a}={b}" for a, b in cfg["aliases"].items()))
        return 0

    if not paths:
        say(f"EMPTY LIBRARY: {library} has no documents yet (.html, .md, .txt, .pdf); save or copy "
            "research into it; nothing loaded")
        return 6
    if not names:
        say("NO SET: name a set, or set `default_set` in the config; known: " + ", ".join(sets))
        return 2
    bad = [n for n in names if n not in sets]
    if bad:
        say(f"unknown set(s): {', '.join(bad)} (names use lowercase letters, digits, . _ -); "
            f"known: {', '.join(sets)}")
        return 2

    alldocs = list(dict.fromkeys(d for n in names for d in sets[n]))
    texts = Texts(paths, store)
    atexit.register(texts.flush)
    missing = []
    for d in alldocs:
        if d not in paths:
            missing.append(f"MISSING  {d} (not in {library})")
        elif d not in texts:
            missing.append(f"UNREADABLE  {paths[d]} ({texts.errors[d]})")
    secs = sections_for(names, sets, kinds, conf, texts, args.setup and not secondary)
    if secondary:
        for s in secs:
            s["remember"] = False  # a second tab never saves a preset unless the user asks
            if s["first"]:
                s["ticked"] = set()  # nor loads a whole set nobody chose yet
    return {"cfg": cfg, "paths": paths, "conf": conf, "names": names, "sets": sets, "kinds": kinds,
            "texts": texts, "alldocs": alldocs, "missing": missing, "secs": secs, "secondary": secondary}


def research_tab(r, names_path, ui_path):
    """The popup tab of the research."""
    chip, by_time = ui_state(ui_path, False)
    texts, sizes, times = r["texts"], {}, {}
    for s in r["secs"]:
        for d in s["main"] + s["other"]:
            if d in texts:
                sizes[d] = len(texts[d])
            try:
                times[d] = int(os.stat(r["paths"][d]).st_mtime)
            except (KeyError, OSError):
                pass
    texts.flush()
    look = {"chip": chip, "by_time": by_time, "times": times,
            "labels": state_names(names_path, "docs"), "titles": state_names(names_path, "sets")}
    return {"sections": r["secs"], "sizes": sizes, "look": look}


def research_finish(r, args, cfg_path, state_path, names_path, ui_path, picks):
    """Save and load what was picked (picks None: no popup, the saved defaults). Returns the exit
    code."""
    cfg, paths, conf, names, sets, kinds = r["cfg"], r["paths"], r["conf"], r["names"], r["sets"], r["kinds"]
    texts, alldocs, secs, secondary = r["texts"], r["alldocs"], r["secs"], r["secondary"]
    setup = args.setup and not secondary
    unsaved = [n for n in names if (secondary or kinds[n] == "custom") and n not in conf]
    if picks is None and unsaved:
        if secondary:
            return 0  # no popup: the second tab loads only a choice saved earlier
        print(f"NO POPUP: {', '.join(unsaved)} has no saved choice yet; nothing loaded")
        return 5
    if picks is None:  # --no-popup or no display: saved defaults, nothing saved
        chosen_picks = {s["name"]: (s["ticked"], False) for s in secs}
    else:
        try:
            save_ui(ui_path, False, picks["chip"], picks["by_time"])
            state_rename(names_path, "docs", picks["renames"])
            state_rename(names_path, "sets", {p["name"]: p["title"] for p in picks["picks"]
                                              if p["title"] is not None})
        except OSError as e:
            print(f"could not save the names or window settings next to {names_path}: {e}")
        chosen_picks = {p["name"]: (p["ticked"], p["remember"]) for p in picks["picks"]}
    if secondary and not any(t for t, _ in chosen_picks.values()):
        return 0  # the other tab, left alone

    order = {s["name"]: s["main"] + s["other"] for s in secs}
    if any(keep or setup for _, keep in chosen_picks.values()):
        conf = load_conf(state_path)  # a save another run made while the window was open stays
    for n, (ticked, keep) in chosen_picks.items():
        if keep or setup:
            conf[n] = {"selected": [d for d in order[n] if d in ticked], "known": list(sets[n])}
            if kinds[n] == "custom":
                conf[n]["custom"] = True
            print(f"SAVED default for {n}: {len(ticked)} of {len(sets[n])} docs")
    if any(keep or setup for _, keep in chosen_picks.values()):
        try:
            save_conf(state_path, conf)
        except OSError as e:
            print(f"could not save the defaults to {state_path}: {e}")
    if setup:
        print("setup only: nothing loaded")
        return 0

    chosen = {d for t, _ in chosen_picks.values() for d in t}
    docs = [d for d in dict.fromkeys(alldocs + [d for s in secs for d in s["main"] + s["other"]])
            if d in chosen and d in texts]
    written = [texts.path(d) for d in docs]
    texts.flush()
    share_note = None
    if docs and usage_enabled(cfg):
        try:  # the log must never break a load
            share_note = finish_usage(cfg, cfg_path, usage_record(docs, texts, paths))
        except OSError:
            pass
    shown = state_names(names_path, "docs")
    total = 0
    for d, dst in zip(docs, written):
        total += len(texts[d])
        if d in shown:
            print(f'DOC "{shown[d]}" ({d})')
        print(f"{len(texts[d]):>7} chars  {dst}")
    for line in r["missing"]:
        print(line)
    skipped = [d for d in alldocs if d in texts and d not in chosen]
    if len(skipped) > SKIPPED_NAMED:  # a custom set or `all`: naming it all costs tokens
        print(f"left out by choice ({len(skipped)}): not named (more than {SKIPPED_NAMED})")
    elif skipped:
        print(f"left out by choice ({len(skipped)}): " + "; ".join(skipped))
    print(f"total: {len(docs)} docs, {total} chars (~{total // CHARS_PER_K}k tokens)")
    if share_note:
        print(share_note)
    return 0


def load_both(args, cfg_path, state_path):
    """A load: research (`researcher SET ...`) or chats (`--context PRESET ...`) in one window with
    a tab for each. The command's own side is ready when the window opens; the other tab (the
    preset `default`, or the config's default_set) is only listed, and loads when the user clicks
    it, so a load never waits for a scan nobody asked for. Without a window only the own side
    loads. The tab the window opens on prints first."""
    conf_dir = os.path.dirname(cfg_path)
    names_path, ui_path = os.path.join(conf_dir, "names.json"), os.path.join(conf_dir, "ui.json")
    cpaths = {"reg": os.path.join(conf_dir, "sessions.json"), "presets": os.path.join(conf_dir, "contexts.json"),
              "ui": ui_path}
    no_popup = args.no_popup or bool(os.environ.get("RESEARCHER_NO_POPUP"))
    chats_first = args.context if args.start is None else args.start == "chats"
    # --setup, --list and --sync work on one side; without a window there is no tab to click
    other_side = not (args.setup or args.list or args.sync or no_popup)
    chat_out = (args.out or cache_home()) if args.context else cache_home()
    research = chats = None
    if args.context:
        try:
            cfg = load_config(cfg_path)
        except ConfigError as e:
            print(f"CONFIG ERROR: {e}")
            return 6
        chats = context_prep(args, cfg, cpaths, args.sets)
        if isinstance(chats, int):
            return chats
    else:
        research = research_prep(args, cfg_path, state_path)
        if isinstance(research, int):
            return research
        cfg = research["cfg"]

    later = {}  # the other side, filled by its loader on the first click of its tab
    own_pick = other_pick = None
    if not no_popup:
        tabs = []
        if chats:
            tabs.append(context_tab(chats, ui_path))
        elif other_side and sessions_settings(cfg) is not None:
            def load_chats():
                c = context_prep(args, cfg, cpaths, [], secondary=True)
                if isinstance(c, int):
                    return None
                later["chats"] = c
                return context_tab(c, ui_path)
            tabs.append({"chats": True, "loader": load_chats})
        if research:
            tabs.append(research_tab(research, names_path, ui_path))
        elif other_side and resolve_names([], cfg):
            def load_research():
                r = research_prep(args, cfg_path, state_path, secondary=True)
                if isinstance(r, int):
                    return None
                later["research"] = r
                return research_tab(r, names_path, ui_path)
            tabs.append({"chats": False, "loader": load_research})
        start = 0 if chats_first else len(tabs) - 1
        if args.context:
            command = "researcher --context " + " ".join(args.sets)
        else:
            command = "researcher " + " ".join(research["names"])
        try:
            picked = popup(tabs, start, args.setup, command.rstrip())
        except RuntimeError as e:
            print(f"NO POPUP ({e}): " + ("using the saved presets" if args.context else "using the saved defaults"))
        else:
            if picked is None:
                print("CANCELLED: the user closed the popup, nothing loaded")
                return 3
            if picked == "timeout":
                print(f"TIMEOUT: popup closed after {POPUP_TIMEOUT} s without a choice, nothing loaded")
                return 4
            # tabs went in as [chats, research]; the own side is the one that was ready
            picked = list(picked) + [None]
            c, r = (picked[0], picked[1]) if len(tabs) > 1 or chats else (None, picked[0])
            own_pick, other_pick = (c, r) if args.context else (r, c)

    def side(chats_turn):
        own = chats_turn == args.context
        pick = own_pick if own else other_pick
        if chats_turn:
            c = chats if own else later.get("chats")
            return context_finish(c, args, cfg, cfg_path, cpaths, chat_out, pick) if c else 0
        r = research if own else later.get("research")
        return research_finish(r, args, cfg_path, state_path, names_path, ui_path, pick) if r else 0
    codes = {t: side(t) for t in ((True, False) if chats_first else (False, True))}
    # the command's own side decides the exit code; the other side only adds to it
    return codes[args.context] or codes[not args.context]


def code_finish(files, ticked):
    """Print the ticked source files (by path: Claude reads them itself). No usage record."""
    n = size = 0
    for doc, (path, nbytes, _) in files.items():
        if doc in ticked:
            print(f"CODE {doc}")
            print(f"{nbytes:>7} bytes  {path}")
            n, size = n + 1, size + nbytes
    for doc in sorted(d for d in ticked if d not in files):
        print(f"MISSING  code {doc} (not under the profile root any more)")
    print(f"total: {n} files, {size} bytes")
    return 0


def code_tab(files, ui_path):
    """The popup tab of a profile's source files."""
    chip, by_time = ui_state(ui_path, False)
    ids = list(files)
    sec = {"name": "code", "kind": "custom", "docs": ids, "main": ids, "other": [], "new": set(),
           "ticked": set(), "remember": False, "first": False}
    return {"sections": [sec], "sizes": {d: f[1] for d, f in files.items()},
            "look": {"code": True, "no_remember": True, "chip": chip, "by_time": by_time,
                     "times": {d: f[2] for d, f in files.items()}}}


def profile_pick(name, ticked, ui_path, chats):
    """A pick of exactly `ticked` in the one section of a profile tab (the window settings as
    saved: finishing the load saves them back unchanged)."""
    chip, by_time = ui_state(ui_path, chats)
    return {"picks": [{"name": name, "ticked": set(ticked), "remember": False, "save_as": name,
                       "title": None}], "renames": {}, "chip": chip, "by_time": by_time}


def profile_research(args, cfg_path, state_path):
    """Research of a profile: the whole library in one section, nothing saved as a set default."""
    a = argparse.Namespace(**vars(args))
    a.sets, a.setup, a.list, a.sync, a.no_popup, a.out = ["all"], False, False, False, True, None
    r = research_prep(a, cfg_path, state_path)
    if isinstance(r, int):
        return None
    for s in r["secs"]:
        s["ticked"], s["remember"] = set(), False
    return r


def load_profile(args, name, cfg_path, state_path):
    """--profile NAME [PRESET]: one window with Code | Chats | Research for a project, and presets
    that tick all three at once. Code is ready at once; chats and research load when their tab
    is clicked or a preset needs them. Without a window a named preset loads as saved."""
    conf_dir = os.path.dirname(cfg_path)
    names_path, ui_path = os.path.join(conf_dir, "names.json"), os.path.join(conf_dir, "ui.json")
    profiles_path = os.path.join(conf_dir, "profiles.json")
    cpaths = {"reg": os.path.join(conf_dir, "sessions.json"), "presets": os.path.join(conf_dir, "contexts.json"),
              "ui": ui_path}
    no_popup = args.no_popup or bool(os.environ.get("RESEARCHER_NO_POPUP"))
    name = name.strip().lower()
    if not PRESET_RE.fullmatch(name):
        print(f"bad profile name: {name} (use lowercase letters, digits, . _ -)")
        return 2
    try:
        cfg = load_config(cfg_path)
    except ConfigError as e:
        print(f"CONFIG ERROR: {e}")
        return 6
    profiles = cfg.get("profiles")
    pcfg = profiles.get(name) if isinstance(profiles, dict) else None
    pcfg = pcfg if isinstance(pcfg, dict) else {}
    root = pcfg.get("root")
    try:
        if root is not None and not isinstance(root, str):
            raise ConfigError(f"[profiles.{name}] root must be a string")
        globs = str_list(pcfg.get("code"), f"[profiles.{name}] code") or ["*"]
    except ConfigError as e:
        print(f"CONFIG ERROR: {e}")
        return 6
    root = os.path.abspath(os.path.expanduser(root)) if root else None
    if root and not os.path.isdir(root):
        print(f"NO ROOT: [profiles.{name}] root = {root} is not a folder; nothing loaded")
        return 6
    presets = profile_presets(profiles_path, name)
    if args.list:
        if not presets:
            print(f"profile {name}: no presets saved yet")
        for p, l in presets:
            print(f"{p:20} {len(l['code']):>4} files {len(l['chats']):>4} chats {len(l['research']):>4} docs")
        return 0
    selected = None
    if args.sets:
        w = args.sets[0].strip().lower()
        selected = next((i for i, (p, _) in enumerate(presets) if p == w), None)
        if selected is None:
            print(f"unknown preset {w} for profile {name}; known: {', '.join(p for p, _ in presets)}")
            return 2
    files = code_scan(root, globs) if root else None
    chat_out = args.out or cache_home()
    sessions_on = sessions_settings(cfg) is not None
    has_library = bool(args.library or os.environ.get("RESEARCHER_LIBRARY") or cfg.get("library"))

    picks = {}  # kind -> pick (code: a set of ids)
    chats = research = None
    save_as = None
    later = {}
    if no_popup:
        if selected is None:
            print(f"NO POPUP: profile {name}: name a preset to load it without a window; nothing loaded")
            return 5
        lists = presets[selected][1]
        if files is not None and lists["code"]:
            picks["code"] = set(lists["code"])
        if sessions_on and lists["chats"]:
            chats = context_prep(args, cfg, cpaths, [name], secondary=True)
            chats = None if isinstance(chats, int) else chats
            picks["chats"] = profile_pick(name, lists["chats"], ui_path, True)
        if has_library and lists["research"]:
            research = profile_research(args, cfg_path, state_path)
            picks["research"] = profile_pick("all", lists["research"], ui_path, False)
    else:
        tabs, kinds = [], []
        if files is not None:
            tabs.append(code_tab(files, ui_path))
            kinds.append("code")
        if sessions_on:
            def load_chats():
                c = context_prep(args, cfg, cpaths, [name], secondary=True)
                if isinstance(c, int):
                    return None
                for s in c["secs"]:
                    s["ticked"] = set()
                tab = context_tab(c, ui_path)
                tab["look"]["no_remember"] = True
                later["chats"] = c
                return tab
            tabs.append({"chats": True, "loader": load_chats})
            kinds.append("chats")
        if has_library:
            def load_research():
                r = profile_research(args, cfg_path, state_path)
                if r is None:
                    return None
                tab = research_tab(r, names_path, ui_path)
                tab["look"]["no_remember"] = True
                tab["look"]["titles"].setdefault("all", f"research for {name}")
                later["research"] = r
                return tab
            tabs.append({"chats": False, "loader": load_research})
            kinds.append("research")
        if not tabs:
            print(f"NOTHING TO SHOW: profile {name} has no root (code), sessions are off and there is no "
                  "library; nothing loaded")
            return 6
        profile = {"name": name, "selected": selected,
                   "presets": [(p, [l[k] for k in kinds]) for p, l in presets]}
        start = kinds.index(args.start) if args.start in kinds else 0
        try:
            picked = popup(tabs, start, False, f"researcher --profile {name} {' '.join(args.sets)}".rstrip(),
                           profile)
        except RuntimeError as e:
            print(f"NO POPUP ({e}): run it with a preset name and --no-popup to load without a window")
            return 5
        if picked is None:
            print("CANCELLED: the user closed the popup, nothing loaded")
            return 3
        if picked == "timeout":
            print(f"TIMEOUT: popup closed after {POPUP_TIMEOUT} s without a choice, nothing loaded")
            return 4
        tab_picks, save_as = picked
        for k, p in zip(kinds, tab_picks):
            if p is not None:
                picks[k] = {d for q in p["picks"] for d in q["ticked"]} if k == "code" else p
        chats, research = later.get("chats"), later.get("research")

    if save_as:
        def ticked(p):
            return [d for q in p["picks"] for d in q["ticked"]] if p else []
        lists = {"code": sorted(picks.get("code") or ()), "chats": ticked(picks.get("chats")),
                 "research": ticked(picks.get("research"))}
        try:
            save_profile_preset(profiles_path, name, save_as, lists)
            print(f"SAVED profile preset {name}/{save_as}: {len(lists['code'])} files, "
                  f"{len(lists['chats'])} chats, {len(lists['research'])} docs")
        except OSError as e:
            print(f"could not save the profile preset to {profiles_path}: {e}")

    # print order: the tab it opened on first, then the bar order (code, chats, research)
    first = args.start or "code"
    order = sorted(("code", "chats", "research"), key=lambda k: k != first)
    code = 0
    for k in order:
        p = picks.get(k)
        if k == "code":
            c = code_finish(files, p) if files is not None and p else 0
        elif k == "chats":
            c = (context_finish(chats, args, cfg, cfg_path, cpaths, chat_out, p)
                 if chats and p and any(q["ticked"] for q in p["picks"]) else 0)
        else:
            c = (research_finish(research, args, cfg_path, state_path, names_path, ui_path, p)
                 if research and p and any(q["ticked"] for q in p["picks"]) else 0)
        code = code or c
    return code


if __name__ == "__main__":
    sys.exit(main())
