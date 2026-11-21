// SPDX-License-Identifier: MIT
// The Researcher. Copyright (c) 2026 LudaLunizz
// Licensed under the MIT License; see the file LICENSE for details.
//! The Researcher: a research loader for Claude (saved research into a Claude Code session).
//!
//! A "library" is a folder of documents (.html, .md, .txt, .pdf). A "set" is a named list of
//! documents from it, defined in config.toml. A popup lets the user tick what to load, so no
//! tokens are spent on documents that are not needed. The ticked documents are read from a
//! plain-text copy in the researcher's own folder (made once per document, redone when the
//! source changes; PDF and HTML are squeezed for tokens) and their paths are printed, so the
//! skill can Read them in full. Nothing in the library is ever changed or moved.
//!
//! Same command line, files and exit codes as `skills/researcher/researcher.py`.

mod bands;
mod code;
mod library;
mod popup;
mod sessions;
mod state;
mod text;
mod usage;
mod util;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};

use clap::Parser;
use serde_json::{Value, json};

use library::{ConfigError, SetKind};
use popup::{FirstUse, Outcome, POPUP_TIMEOUT, TabPick};
use text::Texts;
use usage::CHARS_PER_K;

pub const APP: &str = "researcher";
/// Up to this many left-out documents are named in the output; more are only counted.
const SKIPPED_NAMED: usize = 20;

#[derive(Parser, Clone)]
#[command(name = "researcher", version, about = "Load saved research documents into Claude Code.")]
struct Args {
    #[arg(value_name = "SET")]
    sets: Vec<String>,
    /// change saved defaults, load nothing
    #[arg(long)]
    setup: bool,
    /// use the saved defaults, ask nothing (same as RESEARCHER_NO_POPUP=1)
    #[arg(long)]
    no_popup: bool,
    /// list sets and saved defaults
    #[arg(long)]
    list: bool,
    /// list tracked closed sessions (needs [sessions] enabled = true)
    #[arg(long)]
    sessions: bool,
    /// rename a tracked session
    #[arg(long, num_args = 2, value_names = ["ID", "NAME"], allow_hyphen_values = true)]
    rename: Option<Vec<String>>,
    /// print the command that resumes a session
    #[arg(long, value_name = "ID")]
    resume: Option<String>,
    /// write a tracked session's conversation as a .txt to load as context
    #[arg(long, value_name = "ID")]
    session_load: Option<String>,
    /// pick earlier Claude Code chats (all folders) to load as context; SET names are presets
    #[arg(long)]
    context: bool,
    /// the tab the window opens on (default: chats with --context, code with --profile, research
    /// otherwise)
    #[arg(long, value_name = "code|chats|research", value_parser = ["code", "chats", "research"])]
    start: Option<String>,
    /// a project profile: code, chats and research in one window, with presets; SET = the preset
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
    /// sessions shown by --sessions
    #[arg(long, default_value_t = 30)]
    limit: usize,
    /// write a starter config
    #[arg(long, value_name = "LIBRARY", num_args = 0..=1, default_missing_value = "")]
    init: Option<String>,
    /// config.toml path (default: <config dir>/researcher/config.toml)
    #[arg(long)]
    config: Option<String>,
    /// library folder (overrides config and RESEARCHER_LIBRARY)
    #[arg(long)]
    library: Option<String>,
    /// make or refresh the plain-text copy of the whole library, load nothing
    #[arg(long)]
    sync: bool,
    /// print the local usage totals (characters and estimated tokens saved)
    #[arg(long)]
    usage: bool,
    /// write the anonymous usage records to FILE to look at; nothing is sent
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "researcher-usage.json")]
    usage_export: Option<String>,
    /// share the anonymous usage counts with the project (on) or not (off); saved in the config
    #[arg(long, value_name = "on|off", value_parser = ["on", "off"])]
    usage_share: Option<String>,
    /// send the usage records not sent yet (only when sharing is on)
    #[arg(long)]
    usage_send: bool,
    /// with --usage-send: print what would be sent, send nothing
    #[arg(long, requires = "usage_send")]
    dry_run: bool,
    /// the researcher's text folder (default <data dir>/researcher/text, or text_dir in the
    /// config); session loads go to <cache dir>/researcher
    #[arg(long)]
    out: Option<String>,
}

fn dir_or(d: Option<std::path::PathBuf>, fallback: &str) -> String {
    let base = d.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| util::expanduser(fallback));
    util::join(&base, APP)
}

fn config_home() -> String {
    dir_or(dirs::config_dir(), "~/.config")
}

fn cache_home() -> String {
    dir_or(dirs::cache_dir(), "~/.cache")
}

/// The researcher's own folder: a plain-text copy of every library document.
fn text_home() -> String {
    util::join(&usage::data_home(), "text")
}

const STARTER: &str = r#"# The Researcher: config
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
# `researcher --usage`. `share = true` (or `researcher --usage-share on`) also sends those
# counts to the project; nothing is sent unless you set it. Switch the log off with:
# [usage]
# log = false
# share = false
"#;

fn init_config(path: &str, library: &str) -> i32 {
    if std::path::Path::new(path).exists() {
        println!("{path} already exists; not overwriting");
        return 1;
    }
    let escaped = library.replace('\\', "\\\\").replace('"', "\\\"");
    let res = util::make_parent(path).and_then(|_| std::fs::write(path, STARTER.replace("{library}", &escaped)));
    match res {
        Ok(()) => {
            println!("wrote {path}");
            0
        }
        Err(e) => {
            println!("CONFIG ERROR: {path}: {e}");
            6
        }
    }
}

/// First use (no library in the config): ask for the research folder, save it, then start
/// again so the chooser opens (a process can open only one window loop).
fn first_use(cfg_path: &str) -> i32 {
    match popup::first_use(&util::expanduser("~/research"), cfg_path) {
        Err(e) => {
            println!(
                "NO LIBRARY: set `library` in {cfg_path} (or run --init, or set RESEARCHER_LIBRARY); \
                 no window to ask ({e}); nothing loaded"
            );
            6
        }
        Ok(FirstUse::Cancelled) => {
            println!("CANCELLED: no research folder chosen, nothing loaded");
            3
        }
        Ok(FirstUse::Timeout) => {
            println!("TIMEOUT: popup closed after {POPUP_TIMEOUT} s without a choice, nothing loaded");
            4
        }
        Ok(FirstUse::Folder(dir)) => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                println!("NO LIBRARY: could not make {dir}: {e}; nothing loaded");
                return 6;
            }
            if let Err(e) = set_library(cfg_path, &dir) {
                println!("CONFIG ERROR: could not save `library` in {cfg_path}: {e}");
                return 6;
            }
            println!("LIBRARY SET: {dir} (saved as `library` in {cfg_path})");
            let _ = std::io::stdout().flush();
            let me = std::env::current_exe();
            let status = me.and_then(|exe| {
                Command::new(exe).args(std::env::args_os().skip(1)).env("RESEARCHER_FIRST_USE_DONE", "1").status()
            });
            match status {
                Ok(st) => st.code().unwrap_or(1),
                Err(e) => {
                    println!("could not start the chooser ({e}); run the same command again");
                    0
                }
            }
        }
    }
}

/// Put `library = "DIR"` into the config: a starter config when there is none, otherwise the
/// existing `library` line is replaced or a new one goes on top (comments are kept).
fn set_library(cfg_path: &str, dir: &str) -> Result<(), String> {
    let line = format!("library = \"{}\"", dir.replace('\\', "\\\\").replace('"', "\\\""));
    let body = match std::fs::read_to_string(cfg_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => STARTER.replace("library = \"{library}\"", &line),
        Err(e) => return Err(e.to_string()),
        Ok(old) => {
            let re = regex::Regex::new(r"(?m)^[ \t]*library[ \t]*=.*$").expect("valid");
            let top_level = old.parse::<library::Config>().is_ok_and(|c| c.contains_key("library"));
            if top_level && re.is_match(&old) {
                re.replace(&old, regex::NoExpand(&line)).into_owned()
            } else {
                format!("{line}\n{old}")
            }
        }
    };
    let parsed: library::Config = body.parse().map_err(|e: toml::de::Error| e.to_string())?;
    if parsed.get("library").and_then(toml::Value::as_str) != Some(dir) {
        return Err("the config would not read back the same folder".into());
    }
    util::make_parent(cfg_path).map_err(|e| e.to_string())?;
    util::write_atomic(cfg_path, body.as_bytes()).map_err(|e| e.to_string())
}

/// Put `share = true|false` under `[usage]` in the config (comments and everything else kept).
fn set_share(cfg_path: &str, on: bool) -> Result<(), String> {
    let line = format!("share = {on}");
    let body = match std::fs::read_to_string(cfg_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!("[usage]\n{line}\n"),
        Err(e) => return Err(e.to_string()),
        Ok(old) => {
            let header = regex::Regex::new(r"(?m)^[ \t]*\[usage\][ \t]*(#[^\r\n]*)?\r?$").expect("valid");
            match header.find(&old) {
                // no [usage] table yet: a new one at the end, after a blank line
                None if old.trim().is_empty() => format!("[usage]\n{line}\n"),
                None => {
                    format!("{}\n[usage]\n{line}\n", if old.ends_with('\n') { old.clone() } else { old.clone() + "\n" })
                }
                Some(h) => {
                    let start = old[h.end()..].find('\n').map_or(old.len(), |i| h.end() + i + 1);
                    let next = regex::Regex::new(r"(?m)^[ \t]*\[").expect("valid");
                    let end = next.find(&old[start..]).map_or(old.len(), |m| start + m.start());
                    let key = regex::Regex::new(r"(?m)^[ \t]*share[ \t]*=.*$").expect("valid");
                    let section = &old[start..end];
                    let section = if key.is_match(section) {
                        key.replace(section, regex::NoExpand(&line)).into_owned()
                    } else {
                        format!("{line}\n{section}")
                    };
                    // the header can be the last line, without a newline
                    let head = if old[..start].ends_with('\n') {
                        old[..start].to_string()
                    } else {
                        format!("{}\n", &old[..start])
                    };
                    format!("{head}{section}{}", &old[end..])
                }
            }
        }
    };
    let parsed: library::Config = body.parse().map_err(|e: toml::de::Error| e.to_string())?;
    if usage::share(&parsed) != if on { usage::Share::On } else { usage::Share::Off } {
        return Err("the config would not read back the same answer".into());
    }
    util::make_parent(cfg_path).map_err(|e| e.to_string())?;
    util::write_atomic(cfg_path, body.as_bytes()).map_err(|e| e.to_string())
}

/// `--usage-send`: send the records not sent yet. Exit 9 when sharing is not on.
fn usage_send(cfg: &toml::Table, dry_run: bool) -> i32 {
    let share = usage::share(cfg);
    let url = usage::url(cfg);
    let recs = usage::pending();
    if dry_run {
        println!(
            "DRY RUN: {} records would be sent to {url} (sharing: {}); nothing was sent",
            recs.len(),
            share.label()
        );
        if !recs.is_empty() {
            println!("{}", util::json_indent1(&usage::payload(&recs)));
        }
        return 0;
    }
    if share != usage::Share::On {
        println!("USAGE SHARING: {}; nothing sent (turn it on with: researcher --usage-share on)", share.label());
        return 9;
    }
    if recs.is_empty() {
        println!("NOTHING TO SEND: every usage record was sent already");
        return 0;
    }
    match usage::send(&url, &recs) {
        Ok(d) => {
            let refused = if d.refused > 0 {
                format!(", {} refused by the receiver and skipped", d.refused)
            } else {
                String::new()
            };
            println!("SENT: {} records to {url} ({} new there{refused})", d.sent, d.new);
            0
        }
        Err((d, why)) => {
            println!(
                "SEND FAILED: {why}; {} records sent before that, {} stay queued for the next try",
                d.sent,
                recs.len() - d.sent - d.refused
            );
            1
        }
    }
}

fn main() -> ExitCode {
    // the popup deadline counts from the start, before any scan: the skill's 600 s limit does too
    let _ = popup::remaining();
    let args = Args::parse();
    let code = run(args);
    ExitCode::from(code.clamp(0, 255) as u8)
}

fn run(args: Args) -> i32 {
    let cfg_path = args.config.clone().unwrap_or_else(|| util::join(&config_home(), "config.toml"));
    let cfg_dir = util::dirname(&cfg_path);
    let state_path = util::join(&cfg_dir, "choices.json");

    if args.usage || args.usage_send || args.usage_share.is_some() {
        let cfg = match library::load_config(&cfg_path) {
            Ok(c) => c,
            Err(e) => {
                println!("CONFIG ERROR: {e}");
                return 6;
            }
        };
        if let Some(answer) = &args.usage_share {
            let on = answer == "on";
            return match set_share(&cfg_path, on) {
                Ok(()) if on => {
                    println!(
                        "USAGE SHARING: on (saved as `share = true` under [usage] in {cfg_path}); the anonymous \
                         counts are sent to {} after each load. Undo: researcher --usage-share off",
                        usage::url(&cfg)
                    );
                    0
                }
                Ok(()) => {
                    println!(
                        "USAGE SHARING: off (saved as `share = false` under [usage] in {cfg_path}); nothing is sent"
                    );
                    0
                }
                Err(e) => {
                    println!("CONFIG ERROR: could not save `share` under [usage] in {cfg_path}: {e}");
                    6
                }
            };
        }
        if args.usage_send {
            return usage_send(&cfg, args.dry_run);
        }
        println!("{}", usage::report(&usage::read()));
        println!("{}", usage::share_line(usage::share(&cfg), usage::pending().len()));
        return 0;
    }
    if let Some(dst) = &args.usage_export {
        return match usage::export(dst) {
            Ok(n) => {
                println!("wrote {n} records to {}", util::abspath(dst));
                println!(
                    "each record: day, random install id, document counts per format, character sums \
                     (source bytes, converted, squeezed, whole library). No titles, paths or set names. \
                     Nothing was sent."
                );
                0
            }
            Err(e) => {
                println!("could not write {dst}: {e}");
                1
            }
        };
    }
    if let Some(lib) = &args.init {
        let lib = if lib.is_empty() { util::expanduser("~/research") } else { lib.clone() };
        return init_config(&cfg_path, &lib);
    }

    let session_action = if let Some(r) = &args.rename {
        Some(sessions::Action::Rename { id: r[0].clone(), name: r[1].clone() })
    } else if let Some(id) = &args.resume {
        Some(sessions::Action::Resume { id: id.clone() })
    } else if let Some(id) = &args.session_load {
        Some(sessions::Action::Load { id: id.clone() })
    } else if args.sessions {
        Some(sessions::Action::List { limit: args.limit })
    } else {
        None
    };
    if let Some(action) = session_action {
        let cfg = match library::load_config(&cfg_path) {
            Ok(c) => c,
            Err(e) => {
                println!("CONFIG ERROR: {e}");
                return 6;
            }
        };
        let out = args.out.clone().unwrap_or_else(cache_home);
        return sessions::main(action, &cfg, &cfg_path, &util::join(&cfg_dir, "sessions.json"), &out);
    }

    if let Some(name) = &args.profile {
        return load_profile(&args, name, &cfg_path, &state_path);
    }
    load_both(&args, &cfg_path, &state_path)
}

/// The research side of a load, ready for the popup.
struct Research {
    cfg: library::Config,
    paths: BTreeMap<String, String>,
    conf: library::Choices,
    names: Vec<String>,
    sets: library::Sets,
    texts: Texts,
    alldocs: Vec<String>,
    missing: Vec<String>,
    secs: Vec<library::Section>,
    /// The other tab of a load that started on chats: quiet, and only loaded when ticked.
    secondary: bool,
}

/// Find the research for `researcher SET ...` (or, as the second tab, the config's
/// default_set). Err(code) = stop with that exit code (the message is printed, unless
/// secondary).
fn research_prep(args: &Args, cfg_path: &str, state_path: &str, secondary: bool) -> Result<Research, i32> {
    let say = |m: String| {
        if !secondary {
            println!("{m}");
        }
    };
    let cfg_err = |e: ConfigError| {
        say(format!("CONFIG ERROR: {e}"));
        6
    };
    let cfg = library::load_config(cfg_path).map_err(cfg_err)?;
    let env_lib = std::env::var("RESEARCHER_LIBRARY").ok().filter(|s| !s.is_empty());
    let lib_setting = args.library.clone().filter(|s| !s.is_empty()).or(env_lib);
    let lib_setting = match lib_setting {
        Some(l) => Some(l),
        None => library::cfg_str(&cfg, "library").map_err(cfg_err)?.filter(|s| !s.is_empty()).map(str::to_string),
    };
    let no_popup = args.no_popup || std::env::var_os("RESEARCHER_NO_POPUP").is_some_and(|v| !v.is_empty());
    let Some(lib) = lib_setting.map(|l| util::user_path(&l)) else {
        let asked = std::env::var_os("RESEARCHER_FIRST_USE_DONE").is_some();
        if !(secondary || args.list || args.sync || no_popup || asked) {
            return Err(first_use(cfg_path));
        }
        say(format!(
            "NO LIBRARY: set `library` in {cfg_path} (or run --init, or set RESEARCHER_LIBRARY); nothing loaded"
        ));
        return Err(6);
    };
    if !std::path::Path::new(&lib).is_dir() {
        say(format!("NO LIBRARY: {lib} is not a folder; nothing loaded"));
        return Err(6);
    }
    let paths = library::scan(&lib, &library::str_list(cfg.get("exclude"), "exclude").map_err(cfg_err)?);
    let catalog: Vec<String> = paths.keys().cloned().collect();
    let conf = library::load_choices(state_path);
    // the set names on the command line belong to the tab the command is for
    let names = library::resolve_names(if secondary { &[] } else { &args.sets }, &cfg);
    let sets = library::build_sets(&cfg, &catalog, &conf, &names).map_err(cfg_err)?;

    // the second tab keeps its text copy where it always is; --out is for the first tab
    let out = if secondary { None } else { args.out.clone() };
    let store = match out {
        Some(o) => o,
        None => library::cfg_str(&cfg, "text_dir")
            .map_err(cfg_err)?
            .filter(|s| !s.is_empty())
            .map_or_else(text_home, str::to_string),
    };
    let store = util::user_path(&store);
    // like realpath: resolve the deepest part that exists, so a text folder not made yet is
    // still seen through a symlinked library
    let real = |p: &str| {
        let p = std::path::Path::new(p);
        p.ancestors()
            .find_map(|a| std::fs::canonicalize(a).ok().map(|r| r.join(p.strip_prefix(a).unwrap_or(p))))
            .unwrap_or_else(|| p.to_path_buf())
    };
    if real(&store).starts_with(real(&lib)) {
        // the copies would overwrite the user's own files, or be read as documents again
        return Err(cfg_err(ConfigError(format!(
            "the text folder {store} is inside the library {lib}; set text_dir (or --out) to a folder outside it"
        ))));
    }

    if args.sync && !secondary {
        let mut texts = Texts::new(&paths, &store);
        let ok: Vec<&String> = catalog.iter().filter(|d| texts.has(d)).collect();
        texts.flush();
        let chars: usize = ok.iter().map(|d| texts.len(d)).sum();
        for d in &catalog {
            if !texts.has(d) {
                println!("UNREADABLE  {} ({})", paths[d], texts.error(d));
            }
        }
        println!(
            "text copy: {} of {} docs in {store}, {chars} chars (~{}k tokens)",
            ok.len(),
            paths.len(),
            chars / CHARS_PER_K
        );
        return Err(0);
    }

    if args.list && !secondary {
        println!("library: {lib} ({} documents)", paths.len());
        for n in &sets.order {
            let kind = sets.kinds[n];
            if n == "all" || kind != SetKind::Custom || conf.contains_key(n) {
                let saved = conf.get(n).and_then(|r| r.get("selected")).and_then(Value::as_array);
                let sel = saved.map_or(String::new(), |s| format!(", saved default {}", s.len()));
                println!("{n:20} {:>4} docs  ({}{sel})", sets.docs[n].len(), kind.label());
            }
        }
        if let Some(toml::Value::Table(a)) = cfg.get("aliases")
            && !a.is_empty()
        {
            let pairs: Vec<String> = a
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().map_or_else(|| v.to_string(), str::to_string)))
                .collect();
            println!("aliases: {}", pairs.join(", "));
        }
        return Err(0);
    }

    if paths.is_empty() {
        say(format!(
            "EMPTY LIBRARY: {lib} has no documents yet (.html, .md, .txt, .pdf); save or copy research into it; \
             nothing loaded"
        ));
        return Err(6);
    }
    if names.is_empty() {
        say(format!("NO SET: name a set, or set `default_set` in the config; known: {}", sets.order.join(", ")));
        return Err(2);
    }
    let bad: Vec<&String> = names.iter().filter(|n| !sets.contains(n)).collect();
    if !bad.is_empty() {
        let bad: Vec<&str> = bad.iter().map(|s| s.as_str()).collect();
        say(format!(
            "unknown set(s): {} (names use lowercase letters, digits, . _ -); known: {}",
            bad.join(", "),
            sets.order.join(", ")
        ));
        return Err(2);
    }

    let alldocs: Vec<String> = library::dedupe(names.iter().flat_map(|n| sets.docs[n].iter().cloned()));
    let mut texts = Texts::new(&paths, &store);
    let mut missing = Vec::new();
    for d in &alldocs {
        if !paths.contains_key(d) {
            missing.push(format!("MISSING  {d} (not in {lib})"));
        } else if !texts.has(d) {
            missing.push(format!("UNREADABLE  {} ({})", paths[d], texts.error(d)));
        }
    }
    let mut secs = library::sections_for(&names, &sets, &conf, &mut texts, args.setup && !secondary);
    if secondary {
        for s in &mut secs {
            s.remember = false; // a second tab never saves a preset unless the user asks
            if s.first {
                s.ticked.clear(); // nor loads a whole set nobody chose yet
            }
        }
    }
    Ok(Research { cfg, paths, conf, names, sets, texts, alldocs, missing, secs, secondary })
}

/// The popup tab of the research.
fn research_tab(r: &mut Research, names_path: &str, ui_path: &str) -> popup::Tab {
    let (chip, by_time) = state::ui(ui_path, false);
    let mut look = popup::Look { chip, by_time, ..popup::Look::default() };
    look.labels = state::names(names_path, "docs");
    look.titles = state::names(names_path, "sets");
    let mut sizes = BTreeMap::new();
    for s in &r.secs {
        for d in s.main.iter().chain(&s.other) {
            if r.texts.has(d) {
                sizes.insert(d.clone(), r.texts.len(d));
            }
            if let Some(m) = r.paths.get(d).and_then(|p| std::fs::metadata(p).ok()) {
                look.times.insert(d.clone(), util::mtime_secs(&m) as i64);
            }
        }
    }
    r.texts.flush();
    popup::Tab { sections: r.secs.clone(), sizes, look }
}

/// Save and load what was picked (`picks` None: no popup, the saved defaults). Returns the exit
/// code.
fn research_finish(
    r: Research,
    picks: Option<TabPick>,
    setup: bool,
    cfg_path: &str,
    state_path: &str,
    names_path: &str,
    ui_path: &str,
) -> i32 {
    let Research { cfg, paths, mut conf, names, sets, mut texts, alldocs, missing, secs, secondary } = r;
    let unsaved: Vec<&str> = names
        .iter()
        .filter(|n| (secondary || sets.kinds[*n] == SetKind::Custom) && !conf.contains_key(*n))
        .map(String::as_str)
        .collect();
    if picks.is_none() && !unsaved.is_empty() {
        if secondary {
            return 0; // no popup: the second tab loads only a choice saved earlier
        }
        println!("NO POPUP: {} has no saved choice yet; nothing loaded", unsaved.join(", "));
        return 5;
    }
    let picks = match picks {
        Some(t) => {
            if let Err(e) = state::save_ui(ui_path, false, t.chip, t.by_time) {
                println!("could not save the window settings to {ui_path}: {e}");
            }
            let titles: BTreeMap<String, String> =
                t.picks.iter().filter_map(|p| Some((p.name.clone(), p.title.clone()?))).collect();
            for (key, changes) in [("docs", &t.renames), ("sets", &titles)] {
                if let Err(e) = state::rename(names_path, key, changes) {
                    println!("could not save the names to {names_path}: {e}");
                }
            }
            t.picks.into_iter().map(|p| (p.name, p.ticked, p.remember)).collect()
        }
        // --no-popup or no display: the saved defaults, nothing saved
        None => secs.iter().map(|s| (s.name.clone(), s.ticked.clone(), false)).collect::<Vec<_>>(),
    };
    if secondary && picks.iter().all(|(_, t, _)| t.is_empty()) {
        return 0; // the other tab, left alone
    }

    let order: BTreeMap<&str, Vec<&String>> =
        secs.iter().map(|s| (s.name.as_str(), s.main.iter().chain(&s.other).collect())).collect();
    if picks.iter().any(|(_, _, keep)| *keep || setup) {
        conf = library::load_choices(state_path); // a save another run made while the window was open stays
    }
    let mut saved_any = false;
    for (n, ticked, keep) in &picks {
        if *keep || setup {
            let selected: Vec<&String> = order[n.as_str()].iter().filter(|d| ticked.contains(**d)).copied().collect();
            let mut rec = json!({"selected": selected, "known": sets.docs[n]});
            if sets.kinds[n] == SetKind::Custom {
                rec["custom"] = Value::Bool(true);
            }
            conf.insert(n.clone(), rec);
            println!("SAVED default for {n}: {} of {} docs", ticked.len(), sets.docs[n].len());
            saved_any = true;
        }
    }
    if saved_any && let Err(e) = library::save_choices(state_path, &conf) {
        println!("could not save the defaults to {state_path}: {e}");
    }
    if setup {
        println!("setup only: nothing loaded");
        return 0;
    }

    let chosen: BTreeSet<&String> = picks.iter().flat_map(|(_, t, _)| t.iter()).collect();
    let shown = secs.iter().flat_map(|s| s.main.iter().chain(&s.other)).cloned();
    let docs: Vec<String> = library::dedupe(alldocs.iter().cloned().chain(shown))
        .into_iter()
        .filter(|d| chosen.contains(d) && texts.has(d))
        .collect();
    let written: Vec<String> = docs.iter().map(|d| texts.path(d)).collect();
    texts.flush();
    let mut share_note = None;
    if !docs.is_empty()
        && usage::enabled(&cfg)
        && let Ok(rec) = usage::record(&docs, &mut texts, &paths)
    {
        share_note = usage::finish(&cfg, cfg_path, &rec); // the log must never break a load
    }
    let shown_names = state::names(names_path, "docs");
    let mut total = 0;
    for (d, dst) in docs.iter().zip(&written) {
        let n = texts.len(d);
        total += n;
        if let Some(name) = shown_names.get(d) {
            println!("DOC \"{name}\" ({d})");
        }
        println!("{n:>7} chars  {dst}");
    }
    for line in &missing {
        println!("{line}");
    }
    let skipped: Vec<&str> =
        alldocs.iter().filter(|d| !chosen.contains(d) && texts.has(d)).map(String::as_str).collect();
    if skipped.len() > SKIPPED_NAMED {
        // a custom set or `all` leaves out most of the library: naming it all costs tokens
        println!("left out by choice ({}): not named (more than {SKIPPED_NAMED})", skipped.len());
    } else if !skipped.is_empty() {
        println!("left out by choice ({}): {}", skipped.len(), skipped.join("; "));
    }
    println!("total: {} docs, {total} chars (~{}k tokens)", docs.len(), total / CHARS_PER_K);
    if let Some(note) = share_note {
        println!("{note}");
    }
    0
}

/// A load: research (`researcher SET ...`) or chats (`--context PRESET ...`) in one window with
/// a tab for each. The command's own side is ready when the window opens; the other tab (the
/// preset `default`, or the config's default_set) is only listed, and loads when the user clicks
/// it, so a load never waits for a scan nobody asked for. Without a window only the own side
/// loads. The tab the window opens on prints first.
fn load_both(args: &Args, cfg_path: &str, state_path: &str) -> i32 {
    let cfg_dir = util::dirname(cfg_path);
    let names_path = util::join(&cfg_dir, "names.json");
    let ui_path = util::join(&cfg_dir, "ui.json");
    let reg_path = util::join(&cfg_dir, "sessions.json");
    let presets_path = util::join(&cfg_dir, "contexts.json");
    let no_popup = args.no_popup || std::env::var_os("RESEARCHER_NO_POPUP").is_some_and(|v| !v.is_empty());
    let chats_first = args.start.as_deref().map_or(args.context, |s| s == "chats");
    // --setup, --list and --sync work on one side; without a window there is no tab to click
    let other_side = !(args.setup || args.list || args.sync || no_popup);
    let own_out = if args.context { args.out.clone().unwrap_or_else(cache_home) } else { cache_home() };
    let none: Vec<String> = Vec::new();
    let own_cargs = sessions::ContextArgs {
        names: &args.sets,
        setup: args.setup,
        list: args.list,
        reg_path: &reg_path,
        presets_path: &presets_path,
        out_dir: &own_out,
        cfg_path,
        ui_path: &ui_path,
    };
    let other_cargs = sessions::ContextArgs { names: &none, setup: false, list: false, out_dir: &own_out, ..own_cargs };

    // the command's own side, ready now
    let (cfg, mut research, mut chats) = if args.context {
        let cfg = match library::load_config(cfg_path) {
            Ok(c) => c,
            Err(e) => {
                println!("CONFIG ERROR: {e}");
                return 6;
            }
        };
        match sessions::prep(&cfg, &own_cargs, false) {
            Ok(c) => (cfg, None, Some(c)),
            Err(code) => return code,
        }
    } else {
        match research_prep(args, cfg_path, state_path, false) {
            Ok(r) => (r.cfg.clone(), Some(r), None),
            Err(code) => return code,
        }
    };

    // the other side: a loader that runs on the first click of its tab
    let later_research: Arc<Mutex<Option<Research>>> = Arc::new(Mutex::new(None));
    let later_chats: Arc<Mutex<Option<sessions::Chats>>> = Arc::new(Mutex::new(None));
    let mut r_pick: Option<TabPick> = None;
    let mut c_pick: Option<TabPick> = None;
    let mut other_pick: Option<TabPick> = None;
    if !no_popup {
        let mut tabs: Vec<popup::TabSlot> = Vec::new();
        let other_chats = other_side && !args.context && sessions::settings(&cfg).is_some();
        let other_research = other_side && args.context && !library::resolve_names(&[], &cfg).is_empty();
        if let Some(c) = &chats {
            tabs.push(popup::TabSlot::Ready(Box::new(sessions::tab(c, &ui_path))));
        } else if other_chats {
            let (got, cfg) = (Arc::clone(&later_chats), cfg.clone());
            let (reg, pre, out, cp, ui) =
                (reg_path.clone(), presets_path.clone(), own_out.clone(), cfg_path.to_string(), ui_path.clone());
            let load = move || {
                let none: Vec<String> = Vec::new();
                let a = sessions::ContextArgs {
                    names: &none,
                    setup: false,
                    list: false,
                    reg_path: &reg,
                    presets_path: &pre,
                    out_dir: &out,
                    cfg_path: &cp,
                    ui_path: &ui,
                };
                let c = sessions::prep(&cfg, &a, true).ok()?;
                let tab = sessions::tab(&c, &ui);
                *got.lock().ok()? = Some(c);
                Some(tab)
            };
            tabs.push(popup::TabSlot::Later { chats: true, code: false, load: Box::new(load) });
        }
        if let Some(r) = research.as_mut() {
            tabs.push(popup::TabSlot::Ready(Box::new(research_tab(r, &names_path, &ui_path))));
        } else if other_research {
            let got = Arc::clone(&later_research);
            let (a, cp, sp, np, ui) =
                (args.clone(), cfg_path.to_string(), state_path.to_string(), names_path.clone(), ui_path.clone());
            let load = move || {
                let mut r = research_prep(&a, &cp, &sp, true).ok()?;
                let tab = research_tab(&mut r, &np, &ui);
                *got.lock().ok()? = Some(r);
                Some(tab)
            };
            tabs.push(popup::TabSlot::Later { chats: false, code: false, load: Box::new(load) });
        }
        let start = if chats_first { 0 } else { tabs.len() - 1 };
        let command = if args.context {
            format!("researcher --context {}", args.sets.join(" "))
        } else {
            format!("researcher {}", research.as_ref().map_or(String::new(), |r| r.names.join(" ")))
        };
        let both = tabs.len() > 1;
        match popup::popup(tabs, start, args.setup, command.trim_end(), None) {
            Err(e) if args.context => println!("NO POPUP ({e}): using the saved presets"),
            Err(e) => println!("NO POPUP ({e}): using the saved defaults"),
            Ok(Outcome::Cancelled) => {
                println!("CANCELLED: the user closed the popup, nothing loaded");
                return 3;
            }
            Ok(Outcome::Timeout) => {
                println!("TIMEOUT: popup closed after {POPUP_TIMEOUT} s without a choice, nothing loaded");
                return 4;
            }
            Ok(Outcome::Picked(picked, _)) => {
                // tabs went in as [chats, research]
                let mut it = picked.into_iter();
                let first = it.next().flatten();
                let second = if both { it.next().flatten() } else { None };
                let (c, r) = if both || chats.is_some() { (first, second) } else { (None, first) };
                if args.context {
                    (c_pick, other_pick) = (c, r);
                } else {
                    (other_pick, r_pick) = (c, r);
                }
            }
        }
    }
    // the other tab counts only when it was opened (and so loaded)
    let mut other_research = later_research.lock().ok().and_then(|mut g| g.take());
    let mut other_chats = later_chats.lock().ok().and_then(|mut g| g.take());

    let mut codes = Vec::new(); // (own side?, exit code), in print order
    for chats_turn in if chats_first { [true, false] } else { [false, true] } {
        let own = chats_turn == args.context;
        let code = match (chats_turn, own) {
            (true, true) => chats.take().map_or(0, |c| sessions::finish(c, &cfg, &own_cargs, c_pick.take())),
            (true, false) => {
                other_chats.take().map_or(0, |c| sessions::finish(c, &cfg, &other_cargs, other_pick.take()))
            }
            (false, true) => research.take().map_or(0, |r| {
                research_finish(r, r_pick.take(), args.setup, cfg_path, state_path, &names_path, &ui_path)
            }),
            (false, false) => other_research.take().map_or(0, |r| {
                research_finish(r, other_pick.take(), false, cfg_path, state_path, &names_path, &ui_path)
            }),
        };
        codes.push((own, code));
    }
    // the command's own side decides the exit code; the other side only adds to it
    let own = codes.iter().find(|(o, _)| *o).map_or(0, |(_, c)| *c);
    let other = codes.iter().find(|(o, _)| !*o).map_or(0, |(_, c)| *c);
    if own != 0 { own } else { other }
}

/// Print the ticked source files (by path: Claude reads them itself). No usage record.
fn code_finish(files: &BTreeMap<String, code::File>, ticked: &BTreeSet<String>) -> i32 {
    let mut bytes = 0;
    let mut n = 0;
    for (id, f) in files.iter().filter(|(id, _)| ticked.contains(*id)) {
        println!("CODE {id}");
        println!("{:>7} bytes  {}", f.bytes, f.path);
        bytes += f.bytes;
        n += 1;
    }
    let gone: Vec<&String> = ticked.iter().filter(|id| !files.contains_key(*id)).collect();
    for id in gone {
        println!("MISSING  code {id} (not under the profile root any more)");
    }
    println!("total: {n} files, {bytes} bytes");
    0
}

/// The popup tab of a profile's source files.
fn code_tab(files: &BTreeMap<String, code::File>, ui_path: &str) -> popup::Tab {
    let (chip, by_time) = state::ui(ui_path, false);
    let mut look = popup::Look { code: true, no_remember: true, chip, by_time, ..popup::Look::default() };
    let mut sizes = BTreeMap::new();
    for (id, f) in files {
        look.times.insert(id.clone(), f.mtime);
        sizes.insert(id.clone(), f.bytes as usize);
    }
    let ids: Vec<String> = files.keys().cloned().collect();
    let sec = library::Section {
        name: "code".into(),
        kind: SetKind::Custom,
        docs: ids.clone(),
        main: ids,
        other: Vec::new(),
        new: BTreeSet::new(),
        ticked: BTreeSet::new(),
        remember: false,
        first: false,
    };
    popup::Tab { sections: vec![sec], sizes, look }
}

/// A pick of exactly `ticked` in the one section of a profile tab.
fn profile_pick(name: &str, ticked: BTreeSet<String>, ui_path: &str, chats: bool) -> TabPick {
    let pick = popup::Pick { name: name.into(), ticked, remember: false, save_as: name.into(), title: None };
    // the window settings as saved: finishing the load saves them back unchanged
    let (chip, by_time) = state::ui(ui_path, chats);
    TabPick { picks: vec![pick], renames: BTreeMap::new(), chip, by_time }
}

/// Research of a profile: the whole library in one section, nothing saved as a set default.
fn profile_research(args: &Args, cfg_path: &str, state_path: &str) -> Option<Research> {
    let mut a = args.clone();
    a.sets = vec!["all".into()];
    (a.setup, a.list, a.sync, a.no_popup, a.out) = (false, false, false, true, None); // never the first-use window
    // as the command's own side, so `all` is the set (no first-use window: no_popup is on)
    let mut r = research_prep(&a, cfg_path, state_path, false).ok()?;
    for s in &mut r.secs {
        s.ticked.clear();
        s.remember = false;
    }
    Some(r)
}

/// `--profile NAME [PRESET]`: one window with Code | Chats | Research for a project, and presets
/// that tick all three at once. Code is ready at once; chats and research load when their tab
/// is clicked or a preset needs them. Without a window a named preset loads as saved.
fn load_profile(args: &Args, name: &str, cfg_path: &str, state_path: &str) -> i32 {
    let cfg_dir = util::dirname(cfg_path);
    let (names_path, ui_path) = (util::join(&cfg_dir, "names.json"), util::join(&cfg_dir, "ui.json"));
    let (reg_path, presets_path) = (util::join(&cfg_dir, "sessions.json"), util::join(&cfg_dir, "contexts.json"));
    let profiles_path = util::join(&cfg_dir, "profiles.json");
    let no_popup = args.no_popup || std::env::var_os("RESEARCHER_NO_POPUP").is_some_and(|v| !v.is_empty());
    let name = name.trim().to_lowercase();
    if !popup::valid_preset(&name) {
        println!("bad profile name: {name} (use lowercase letters, digits, . _ -)");
        return 2;
    }
    let cfg = match library::load_config(cfg_path) {
        Ok(c) => c,
        Err(e) => {
            println!("CONFIG ERROR: {e}");
            return 6;
        }
    };
    let pcfg = cfg.get("profiles").and_then(|p| p.get(&name)).and_then(toml::Value::as_table);
    let root = match pcfg.and_then(|p| p.get("root")) {
        None => None,
        Some(toml::Value::String(r)) => Some(util::user_path(r)),
        Some(_) => {
            println!("CONFIG ERROR: [profiles.{name}] root must be a string");
            return 6;
        }
    };
    let globs = match library::str_list(pcfg.and_then(|p| p.get("code")), &format!("[profiles.{name}] code")) {
        Ok(g) if g.is_empty() => vec!["*".to_string()],
        Ok(g) => g,
        Err(e) => {
            println!("CONFIG ERROR: {e}");
            return 6;
        }
    };
    if let Some(r) = root.as_deref().filter(|r| !std::path::Path::new(r).is_dir()) {
        println!("NO ROOT: [profiles.{name}] root = {r} is not a folder; nothing loaded");
        return 6;
    }
    let presets = state::profile_presets(&profiles_path, &name);
    if args.list {
        if presets.is_empty() {
            println!("profile {name}: no presets saved yet");
        }
        for (p, l) in &presets {
            println!(
                "{p:20} {:>4} files {:>4} chats {:>4} docs",
                l["code"].len(),
                l["chats"].len(),
                l["research"].len()
            );
        }
        return 0;
    }
    let wanted = args.sets.first().map(|p| p.trim().to_lowercase());
    let selected = match &wanted {
        None => None,
        Some(w) => match presets.iter().position(|(p, _)| p == w) {
            Some(i) => Some(i),
            None => {
                let known: Vec<&str> = presets.iter().map(|(p, _)| p.as_str()).collect();
                println!("unknown preset {w} for profile {name}; known: {}", known.join(", "));
                return 2;
            }
        },
    };
    let files = root.as_deref().map(|r| code::scan(r, &globs));
    let chat_out = args.out.clone().unwrap_or_else(cache_home);
    let pname = vec![name.clone()];
    let cargs = sessions::ContextArgs {
        names: &pname,
        setup: false,
        list: false,
        reg_path: &reg_path,
        presets_path: &presets_path,
        out_dir: &chat_out,
        cfg_path,
        ui_path: &ui_path,
    };
    let sessions_on = sessions::settings(&cfg).is_some();
    let has_library = args.library.is_some()
        || std::env::var("RESEARCHER_LIBRARY").is_ok_and(|l| !l.is_empty())
        || library::cfg_str(&cfg, "library").ok().flatten().is_some_and(|l| !l.is_empty());

    // tabs in bar order: Code | Chats | Research; picks come back the same way
    let mut code_pick: Option<BTreeSet<String>> = None;
    let mut chat_pick: Option<TabPick> = None;
    let mut research_pick: Option<TabPick> = None;
    let mut chats: Option<sessions::Chats> = None;
    let mut research: Option<Research> = None;
    let mut save_as: Option<String> = None;

    if no_popup {
        let Some(i) = selected else {
            println!("NO POPUP: profile {name}: name a preset to load it without a window; nothing loaded");
            return 5;
        };
        let lists = &presets[i].1;
        let set = |k: &str| -> BTreeSet<String> { lists[k].iter().cloned().collect() };
        if files.is_some() && !lists["code"].is_empty() {
            code_pick = Some(set("code"));
        }
        if sessions_on && !lists["chats"].is_empty() {
            chats = sessions::prep(&cfg, &cargs, true).ok();
            chat_pick = Some(profile_pick(&name, set("chats"), &ui_path, true));
        }
        if has_library && !lists["research"].is_empty() {
            research = profile_research(args, cfg_path, state_path);
            research_pick = Some(profile_pick("all", set("research"), &ui_path, false));
        }
    } else {
        let later_chats: Arc<Mutex<Option<sessions::Chats>>> = Arc::new(Mutex::new(None));
        let later_research: Arc<Mutex<Option<Research>>> = Arc::new(Mutex::new(None));
        let mut tabs: Vec<popup::TabSlot> = Vec::new();
        let mut kinds: Vec<&str> = Vec::new();
        if let Some(f) = &files {
            tabs.push(popup::TabSlot::Ready(Box::new(code_tab(f, &ui_path))));
            kinds.push("code");
        }
        if sessions_on {
            let (got, cfg) = (Arc::clone(&later_chats), cfg.clone());
            let (reg, pre, out, cp, ui, n) = (
                reg_path.clone(),
                presets_path.clone(),
                chat_out.clone(),
                cfg_path.to_string(),
                ui_path.clone(),
                pname.clone(),
            );
            let load = move || {
                let a = sessions::ContextArgs {
                    names: &n,
                    setup: false,
                    list: false,
                    reg_path: &reg,
                    presets_path: &pre,
                    out_dir: &out,
                    cfg_path: &cp,
                    ui_path: &ui,
                };
                let mut c = sessions::prep(&cfg, &a, true).ok()?;
                for s in &mut c.secs {
                    s.ticked.clear();
                }
                let mut tab = sessions::tab(&c, &ui);
                tab.look.no_remember = true;
                *got.lock().ok()? = Some(c);
                Some(tab)
            };
            tabs.push(popup::TabSlot::Later { chats: true, code: false, load: Box::new(load) });
            kinds.push("chats");
        }
        if has_library {
            let got = Arc::clone(&later_research);
            let (a, cp, sp, np, ui, title) = (
                args.clone(),
                cfg_path.to_string(),
                state_path.to_string(),
                names_path.clone(),
                ui_path.clone(),
                format!("research for {name}"),
            );
            let load = move || {
                let mut r = profile_research(&a, &cp, &sp)?;
                let mut tab = research_tab(&mut r, &np, &ui);
                tab.look.no_remember = true;
                tab.look.titles.entry("all".into()).or_insert(title);
                *got.lock().ok()? = Some(r);
                Some(tab)
            };
            tabs.push(popup::TabSlot::Later { chats: false, code: false, load: Box::new(load) });
            kinds.push("research");
        }
        if tabs.is_empty() {
            println!(
                "NOTHING TO SHOW: profile {name} has no root (code), sessions are off and there is no library; \
                 nothing loaded"
            );
            return 6;
        }
        let profile = popup::ProfileUi {
            name: name.clone(),
            presets: presets
                .iter()
                .map(|(p, l)| (p.clone(), kinds.iter().map(|k| l[*k].iter().cloned().collect()).collect()))
                .collect(),
            selected,
        };
        let start = args.start.as_deref().and_then(|s| kinds.iter().position(|k| *k == s)).unwrap_or(0);
        let command = format!("researcher --profile {name} {}", args.sets.join(" "));
        match popup::popup(tabs, start, false, command.trim_end(), Some(profile)) {
            Err(e) => {
                println!("NO POPUP ({e}): run it with a preset name and --no-popup to load without a window");
                return 5;
            }
            Ok(Outcome::Cancelled) => {
                println!("CANCELLED: the user closed the popup, nothing loaded");
                return 3;
            }
            Ok(Outcome::Timeout) => {
                println!("TIMEOUT: popup closed after {POPUP_TIMEOUT} s without a choice, nothing loaded");
                return 4;
            }
            Ok(Outcome::Picked(picked, save)) => {
                save_as = save;
                for (k, p) in kinds.iter().zip(picked) {
                    match *k {
                        "code" => code_pick = p.map(|t| t.picks.into_iter().flat_map(|p| p.ticked).collect()),
                        "chats" => chat_pick = p,
                        _ => research_pick = p,
                    }
                }
            }
        }
        chats = later_chats.lock().ok().and_then(|mut g| g.take());
        research = later_research.lock().ok().and_then(|mut g| g.take());
    }

    if let Some(preset) = &save_as {
        let ticked = |p: &Option<TabPick>| -> Vec<String> {
            p.as_ref().map_or_else(Vec::new, |t| t.picks.iter().flat_map(|p| p.ticked.iter().cloned()).collect())
        };
        let lists = BTreeMap::from([
            ("code".to_string(), code_pick.iter().flatten().cloned().collect::<Vec<_>>()),
            ("chats".to_string(), ticked(&chat_pick)),
            ("research".to_string(), ticked(&research_pick)),
        ]);
        match state::save_profile_preset(&profiles_path, &name, preset, &lists) {
            Ok(()) => println!(
                "SAVED profile preset {name}/{preset}: {} files, {} chats, {} docs",
                lists["code"].len(),
                lists["chats"].len(),
                lists["research"].len()
            ),
            Err(e) => println!("could not save the profile preset to {profiles_path}: {e}"),
        }
    }

    // print order: the tab it opened on first, then the bar order (code, chats, research)
    let first = args.start.as_deref().unwrap_or("code");
    let mut order = vec!["code", "chats", "research"];
    order.sort_by_key(|k| *k != first);
    let mut code = 0;
    for k in order {
        let c = match k {
            "code" => match (&files, code_pick.take()) {
                (Some(f), Some(t)) if !t.is_empty() => code_finish(f, &t),
                _ => 0,
            },
            "chats" => match (chats.take(), chat_pick.take()) {
                (Some(c), Some(p)) if p.picks.iter().any(|p| !p.ticked.is_empty()) => {
                    sessions::finish(c, &cfg, &cargs, Some(p))
                }
                _ => 0,
            },
            _ => match (research.take(), research_pick.take()) {
                (Some(r), Some(p)) if p.picks.iter().any(|p| !p.ticked.is_empty()) => {
                    research_finish(r, Some(p), false, cfg_path, state_path, &names_path, &ui_path)
                }
                _ => 0,
            },
        };
        if code == 0 {
            code = c;
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::set_library;

    fn tmp(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("researcher-unit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml").to_string_lossy().into_owned()
    }

    fn lib_of(path: &str) -> String {
        let c: toml::Table = std::fs::read_to_string(path).unwrap().parse().unwrap();
        c["library"].as_str().unwrap().to_string()
    }

    #[test]
    fn new_config_is_the_starter() {
        let p = tmp("new");
        set_library(&p, "/r/my \"odd\" \\ dir").unwrap();
        assert_eq!(lib_of(&p), "/r/my \"odd\" \\ dir");
        assert!(std::fs::read_to_string(&p).unwrap().contains("# [sets.rust]"));
    }

    #[test]
    fn existing_config_keeps_its_comments_and_sets() {
        let p = tmp("keep");
        std::fs::write(&p, "# mine\ndefault_set = \"a\"\n[sets.a]\ndocs = [\"x\"]\nlibrary = \"in a table\"\n")
            .unwrap();
        set_library(&p, "/r").unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.starts_with("library = \"/r\"\n# mine"));
        assert!(body.contains("library = \"in a table\""));
        assert_eq!(lib_of(&p), "/r");
    }

    #[test]
    fn empty_library_line_is_replaced() {
        let p = tmp("empty");
        std::fs::write(&p, "library = \"\"\ndefault_set = \"a\"\n").unwrap();
        set_library(&p, "/r").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "library = \"/r\"\ndefault_set = \"a\"\n");
    }
}
