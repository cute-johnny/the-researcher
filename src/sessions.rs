// SPDX-License-Identifier: MIT
//! Closed Claude Code sessions (opt-in): a registry keyed by the FULL session id, so a name can
//! never be separated from the id it belongs to. Names come from Claude Code's own title or the
//! first prompt, and can be renamed (a manual name is never overwritten by a rescan).

use std::fs;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{Map, Value, json};

use std::collections::BTreeSet;

use crate::library::{self, Choices, Config, ConfigError, Section, SetKind};
use crate::popup;
use crate::usage::CHARS_PER_K;
use crate::util::{self, chars, py_strip};

static SESSION_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").expect("valid"));
/// opencode session ids: `ses_` and letters/digits (case matters).
static OC_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ses_[A-Za-z0-9]+$").expect("valid"));
static WS_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\x1c-\x1f]+").expect("valid"));
const NAME_MAX: usize = 80;
/// Characters; a longer session keeps its start and its end.
const SESSION_LOAD_MAX: usize = 200_000;

pub struct Settings {
    idle_minutes: f64,
    projects_dir: String,
    /// Also list opencode's chats (opt-in, asks the `opencode` program).
    opencode: bool,
    opencode_cmd: String,
}

/// The `[sessions]` settings, or None when sessions are off.
pub fn settings(cfg: &Config) -> Option<Settings> {
    let Some(toml::Value::Table(sec)) = cfg.get("sessions") else { return None };
    if !util::truthy_toml(sec.get("enabled")) {
        return None;
    }
    let idle_minutes = match sec.get("idle_minutes") {
        Some(toml::Value::Integer(i)) => *i as f64,
        Some(toml::Value::Float(f)) => *f,
        Some(toml::Value::String(s)) => s.trim().parse().unwrap_or(10.0),
        _ => 10.0,
    };
    let dir = match sec.get("projects_dir") {
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "~/.claude/projects".into(),
    };
    let opencode = util::truthy_toml(sec.get("opencode"));
    let opencode_cmd = match sec.get("opencode_cmd") {
        Some(toml::Value::String(c)) if !c.trim().is_empty() => util::expanduser(c.trim()),
        _ => "opencode".into(),
    };
    Some(Settings { idle_minutes, projects_dir: util::user_path(&dir), opencode, opencode_cmd })
}

fn clean_name(text: &str) -> String {
    let t = WS_RUN.replace_all(text, " ");
    let t = py_strip(&t);
    if chars(t) <= NAME_MAX {
        t.to_string()
    } else {
        format!("{}…", util::py_rstrip(util::take_chars(t, NAME_MAX - 1)))
    }
}

/// The plain text of a message: a string, or the text blocks of a list (tools skipped).
fn block_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Entries of a session .jsonl; unreadable or broken lines are skipped.
fn read_transcript(path: &str) -> Vec<Map<String, Value>> {
    let Ok(bytes) = fs::read(path) else { return Vec::new() };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| match serde_json::from_str::<Value>(l) {
            Ok(Value::Object(m)) => Some(m),
            _ => None,
        })
        .collect()
}

fn s<'a>(e: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    e.get(k).and_then(Value::as_str)
}

fn message_text(e: &Map<String, Value>) -> String {
    block_text(e.get("message").and_then(Value::as_object).and_then(|m| m.get("content")))
}

static PASTE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"</?pasted_content[^>]*>").unwrap());

/// The user's own prompt of an entry, or "" (tool results, meta and system text, commands).
fn real_prompt(e: &Map<String, Value>) -> String {
    if s(e, "type") != Some("user")
        || ["isMeta", "isCompactSummary", "isVisibleInTranscriptOnly"].iter().any(|k| util::truthy_json(e.get(*k)))
    {
        return String::new(); // a compaction summary is Claude Code's text, not the user's
    }
    // a large paste is the user's own prompt, wrapped in <pasted_content id="..."> tags
    let text = PASTE_TAG.replace_all(&message_text(e), "").into_owned();
    let text = py_strip(&text);
    if text.is_empty() || text.starts_with('<') { String::new() } else { text.to_string() }
}

#[derive(Default)]
struct Info {
    title: Option<String>,
    first: Option<String>,
    cwd: Option<String>,
    started: Option<String>,
    last: Option<String>,
}

fn session_info(entries: &[Map<String, Value>]) -> Info {
    let mut i = Info::default();
    for e in entries {
        match s(e, "type") {
            Some("custom-title") if s(e, "customTitle").is_some_and(|t| !t.is_empty()) => {
                i.title = s(e, "customTitle").map(str::to_string);
            }
            Some("ai-title") if s(e, "aiTitle").is_some_and(|t| !t.is_empty()) && i.title.is_none() => {
                i.title = s(e, "aiTitle").map(str::to_string);
            }
            _ => {}
        }
        if i.first.is_none() {
            i.first = Some(real_prompt(e)).filter(|p| !p.is_empty());
        }
        if i.cwd.as_deref().is_none_or(str::is_empty) {
            i.cwd = s(e, "cwd").map(str::to_string);
        }
        if let Some(ts) = s(e, "timestamp") {
            if i.started.is_none() {
                i.started = Some(ts.to_string());
            }
            i.last = Some(ts.to_string());
        }
    }
    i
}

fn load_registry(path: &str) -> Map<String, Value> {
    let reg = library::load_choices(path);
    if reg.get("sessions").is_some_and(Value::is_object) {
        return reg;
    }
    if !reg.is_empty() {
        util::set_aside(path);
    }
    json!({"version": 1, "sessions": {}}).as_object().cloned().expect("object")
}

fn save_registry(path: &str, reg: &Map<String, Value>) -> std::io::Result<()> {
    util::make_parent(path)?;
    util::write_atomic(path, util::json_indent1(&Value::Object(reg.clone())).as_bytes())
}

fn sessions_mut(reg: &mut Map<String, Value>) -> &mut Map<String, Value> {
    reg.get_mut("sessions").and_then(Value::as_object_mut).expect("checked by load_registry")
}

fn sessions(reg: &Map<String, Value>) -> &Map<String, Value> {
    reg.get("sessions").and_then(Value::as_object).expect("checked by load_registry")
}

fn opt(v: &Option<String>) -> Value {
    v.as_ref().map_or(Value::Null, |s| Value::String(s.clone()))
}

/// Add or refresh the registry record of one session from its transcript. Returns 1 when the
/// session is new, 0 when it was known or nothing was ever asked.
fn register(
    reg: &mut Map<String, Value>,
    sid: &str,
    path: &str,
    mtime: f64,
    proj: &str,
    source: Option<&str>,
    entries: &[Map<String, Value>],
) -> usize {
    let mut info = session_info(entries);
    let Some(base) = info.title.clone().or_else(|| info.first.clone()) else {
        return 0; // nothing was ever asked
    };
    // a chat started from a command or skill opens with its whole expanded text: opencode's own
    // title is the better label
    if source == Some("opencode") && info.title.is_some() {
        info.first.clone_from(&info.title);
    }
    let auto = clean_name(&base);
    let map = sessions_mut(reg);
    let mut added = 0;
    let rec = match map.get_mut(sid).and_then(Value::as_object_mut) {
        Some(rec) => {
            if s(rec, "name_source") != Some("manual") {
                rec.insert("name".into(), Value::String(auto));
            }
            rec
        }
        None => {
            added = 1;
            let registered = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            map.insert(sid.to_string(), json!({"name": auto, "name_source": "auto", "registered": registered}));
            map.get_mut(sid).and_then(Value::as_object_mut).expect("just inserted")
        }
    };
    rec.insert("file".into(), Value::String(path.to_string()));
    rec.insert("mtime".into(), json!(mtime));
    rec.insert("project".into(), Value::String(proj.to_string()));
    rec.insert("cwd".into(), opt(&info.cwd));
    rec.insert("started".into(), opt(&info.started));
    rec.insert("last".into(), opt(&info.last));
    rec.insert("first".into(), opt(&info.first.as_deref().map(clean_name)));
    if let Some(src) = source {
        rec.insert("source".into(), Value::String(src.to_string()));
    }
    let n = chars(&session_text(sid, &Value::Object(rec.clone()), entries));
    rec.insert("chars".into(), json!(n));
    added
}

/// True when the registry already holds this version of the session's transcript.
fn up_to_date(reg: &Map<String, Value>, sid: &str, path: &str, mtime: f64) -> bool {
    sessions(reg).get(sid).and_then(Value::as_object).is_some_and(|r| {
        r.get("mtime").and_then(Value::as_f64) == Some(mtime)
            && s(r, "file") == Some(path)
            && r.contains_key("first")
            && r.contains_key("chars")
    })
}

/// Add sessions untouched for `idle_secs` to the registry and refresh the changed ones (never
/// `skip`, the session that runs this). Returns the number added. opencode chats are added too
/// when `[sessions] opencode` is on; `in_opencode` = this runs inside opencode (its newest chat
/// is the one that runs this).
fn scan_sessions(
    reg: &mut Map<String, Value>,
    st: &Settings,
    idle_secs: f64,
    skip: &str,
    reg_path: &str,
    in_opencode: bool,
    live_out: &mut Option<String>,
) -> usize {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let mut added = 0;
    if let Ok(rd) = fs::read_dir(&st.projects_dir) {
        let mut projects: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        projects.sort();
        for proj in projects {
            let pdir = util::join(&st.projects_dir, &proj);
            let Ok(rd) = fs::read_dir(&pdir) else { continue };
            let mut files: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            files.sort();
            for f in files {
                let Some(sid) = f.strip_suffix(".jsonl") else { continue };
                if !SESSION_ID_RE.is_match(sid) || sid == skip {
                    continue;
                }
                let path = util::join(&pdir, &f);
                let Ok(meta) = fs::metadata(&path) else { continue };
                let mtime = util::mtime_secs(&meta);
                if now - mtime < idle_secs {
                    continue; // probably still open
                }
                if up_to_date(reg, sid, &path, mtime) {
                    continue;
                }
                added += register(reg, sid, &path, mtime, &proj, None, &read_transcript(&path));
            }
        }
    }
    if st.opencode {
        added += scan_opencode(reg, st, idle_secs, reg_path, in_opencode, live_out);
    }
    added
}

// ---------------------------------------------------------------- opencode chats

/// Run the opencode program and return its standard output.
fn oc_run(cmd: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() { Ok(out.stdout) } else { Err(format!("{cmd} {} failed", args.join(" "))) }
}

fn iso_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

/// The text parts of an opencode message (tool calls, reasoning, synthetic and ignored text left out).
fn oc_text(parts: Option<&Value>) -> String {
    let texts: Vec<&str> = parts
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter(|p| !util::truthy_json(p.get("synthetic")) && !util::truthy_json(p.get("ignored")))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    py_strip(&texts.join("\n")).to_string()
}

/// `opencode export` as transcript entries in the shape of a Claude Code .jsonl (title, user and
/// assistant text with timestamps), so the rest of the program reads both the same way.
fn oc_entries(export: &Value, dir: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let title = export.pointer("/info/title").and_then(Value::as_str).unwrap_or("");
    if !title.is_empty() && !title.starts_with("New session - ") && !title.starts_with('<') {
        out.push(json!({"type": "ai-title", "aiTitle": title}));
    }
    for m in export.get("messages").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
        let role = m.pointer("/info/role").and_then(Value::as_str).unwrap_or("");
        let text = oc_text(m.get("parts"));
        if text.is_empty() || !matches!(role, "user" | "assistant") {
            continue;
        }
        let ms = m.pointer("/info/time/created").and_then(Value::as_i64).unwrap_or(0);
        let content = if role == "user" { json!(text) } else { json!([{"type": "text", "text": text}]) };
        out.push(
            json!({"type": role, "timestamp": iso_ms(ms), "cwd": dir, "message": {"role": role, "content": content}}),
        );
    }
    out
}

/// Add opencode's chats (top-level ones; sub-agent sessions are not listed by opencode) to the
/// registry. Each changed chat is exported once and cached as a transcript next to the registry.
fn scan_opencode(
    reg: &mut Map<String, Value>,
    st: &Settings,
    idle_secs: f64,
    reg_path: &str,
    in_opencode: bool,
    live_out: &mut Option<String>,
) -> usize {
    let list = match oc_run(&st.opencode_cmd, &["session", "list", "--format", "json"]) {
        Ok(b) => serde_json::from_slice::<Value>(&b).ok().and_then(|v| v.as_array().cloned()),
        Err(e) => {
            println!("OPENCODE SKIPPED ({e}): opencode chats left out");
            return 0;
        }
    };
    let Some(list) = list else {
        println!("OPENCODE SKIPPED (unreadable session list): opencode chats left out");
        return 0;
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let upd = |v: &Value| v.get("updated").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
    // inside opencode, the chat that runs this is the newest one, still being written
    let live = if in_opencode {
        list.iter()
            .filter(|v| now - upd(v) < 600.0)
            .max_by(|a, b| upd(a).total_cmp(&upd(b)))
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
    } else {
        None
    };
    *live_out = live.map(str::to_string); // an earlier scan from Claude Code may have registered it
    let cache = util::join(&util::dirname(reg_path), "sessions-opencode");
    let mut todo: Vec<(String, f64, String, String)> = Vec::new(); // id, mtime, directory, cache file
    let mut added = 0;
    for v in &list {
        let Some(sid) = v.get("id").and_then(Value::as_str).filter(|i| OC_ID_RE.is_match(i)) else { continue };
        let mtime = upd(v);
        if Some(sid) == live || now - mtime < idle_secs {
            continue;
        }
        let path = util::join(&cache, &format!("{sid}.jsonl"));
        if up_to_date(reg, sid, &path, mtime) && fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            continue;
        }
        if fs::metadata(&path).is_ok_and(|m| m.is_file() && util::mtime_secs(&m) >= mtime) {
            // exported after its last change (an empty chat never gets a record): read the copy
            added += register(reg, sid, &path, mtime, "opencode", Some("opencode"), &read_transcript(&path));
            continue;
        }
        let dir = v.get("directory").and_then(Value::as_str).unwrap_or("").to_string();
        todo.push((sid.to_string(), mtime, dir, path));
    }
    // opencode takes about two seconds per export: four at a time
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<Option<Value>>> = Mutex::new(vec![None; todo.len()]);
    std::thread::scope(|sc| {
        for _ in 0..4.min(todo.len()) {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((sid, ..)) = todo.get(i) else { break };
                    let ex = oc_run(&st.opencode_cmd, &["export", sid])
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                    if let Ok(mut d) = done.lock() {
                        d[i] = ex;
                    }
                }
            });
        }
    });
    let done = done.into_inner().unwrap_or_default();
    for ((sid, mtime, dir, path), ex) in todo.iter().zip(done) {
        let Some(ex) = ex else { continue };
        let entries = oc_entries(&ex, dir);
        let body: Vec<String> = entries.iter().map(Value::to_string).collect();
        if util::make_parent(path).is_err() || util::write_atomic(path, (body.join("\n") + "\n").as_bytes()).is_err() {
            continue;
        }
        let maps: Vec<Map<String, Value>> =
            entries.into_iter().filter_map(|e| if let Value::Object(m) = e { Some(m) } else { None }).collect();
        added += register(reg, sid, path, *mtime, "opencode", Some("opencode"), &maps);
    }
    added
}

/// A registry id from a full id or a unique prefix (4+ characters).
fn find_session(reg: &Map<String, Value>, ident: &str) -> Result<String, ConfigError> {
    let ident = py_strip(ident).to_lowercase();
    let map = sessions(reg);
    // opencode ids mix upper and lower case: match without case, answer with the real id
    if let Some(k) = map.keys().find(|k| k.to_lowercase() == ident) {
        return Ok(k.clone());
    }
    let hits: Vec<&String> =
        map.keys().filter(|i| chars(&ident) >= 4 && i.to_lowercase().starts_with(&ident)).collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => Err(ConfigError(format!("no session matches '{ident}'"))),
        n => Err(ConfigError(format!("'{ident}' matches {n} sessions; give more of the id"))),
    }
}

fn rec_name(rec: &Value) -> &str {
    rec.get("name").and_then(Value::as_str).unwrap_or("")
}

/// Name and full id, always together.
fn label(sid: &str, rec: &Value) -> String {
    format!("\"{}\" ({sid})", rec_name(rec))
}

fn when(rec: &Value) -> String {
    let t = ["last", "started"]
        .iter()
        .filter_map(|k| rec.get(*k).and_then(Value::as_str))
        .find(|t| !t.is_empty())
        .unwrap_or("");
    util::take_chars(t, 16).replace('T', " ")
}

fn session_text(sid: &str, rec: &Value, entries: &[Map<String, Value>]) -> String {
    let get = |k: &str| rec.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let folder = Some(get("cwd")).filter(|c| !c.is_empty()).unwrap_or_else(|| get("project"));
    let mut parts = vec![
        format!("SESSION: {}", rec_name(rec)),
        format!("SESSION ID: {sid}"),
        format!("PROJECT FOLDER: {folder}"),
        format!("LAST ACTIVE: {}", util::take_chars(&get("last"), 16).replace('T', " ")),
        String::new(),
    ];
    for e in entries {
        let (who, text) = match s(e, "type") {
            Some("user") => ("USER", real_prompt(e)),
            Some("assistant") => ("ASSISTANT", py_strip(&message_text(e)).to_string()),
            _ => continue,
        };
        if !text.is_empty() {
            parts.push(format!("{who}: {text}\n"));
        }
    }
    let body = parts.join("\n");
    let n = chars(&body);
    if n <= SESSION_LOAD_MAX {
        return body;
    }
    let keep = SESSION_LOAD_MAX / 5;
    format!(
        "{}\n\n[... {} characters left out of the middle ...]\n\n{}",
        util::take_chars(&body, keep),
        n - SESSION_LOAD_MAX,
        util::last_chars(&body, SESSION_LOAD_MAX - keep)
    )
}

/// Write one .txt per (name, text); only files this tool wrote earlier are removed first.
fn write_outputs(out_dir: &str, docs: &[(String, String, String)]) -> std::io::Result<Vec<String>> {
    fs::create_dir_all(out_dir)?;
    let manifest = util::join(out_dir, ".manifest.json");
    let old: Vec<String> = fs::read_to_string(&manifest)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default();
    for f in old {
        if !f.contains(['/', '\\']) && !f.is_empty() && f != "." && f != ".." {
            let _ = fs::remove_file(util::join(out_dir, &f)); // never follow a path out of the folder
        }
    }
    let mut written = Vec::new();
    for (d, src, text) in docs {
        let f = crate::text::store_name(d);
        fs::write(util::join(out_dir, &f), format!("SOURCE: {src}\n\n{text}\n"))?;
        written.push(f);
    }
    fs::write(&manifest, serde_json::to_string(&written).unwrap_or_default())?;
    Ok(written.iter().map(|f| util::join(out_dir, f)).collect())
}

pub enum Action {
    List { limit: usize },
    Rename { id: String, name: String },
    Resume { id: String },
    Load { id: String },
}

pub fn main(action: Action, cfg: &Config, cfg_path: &str, reg_path: &str, out_dir: &str) -> i32 {
    let Some(st) = settings(cfg) else {
        println!(
            "SESSIONS OFF: add `[sessions]` with `enabled = true` to the config to track closed sessions; nothing done"
        );
        return 7;
    };
    let mut reg = load_registry(reg_path);
    let in_oc = std::env::var_os("OPENCODE").is_some();
    let added = scan_sessions(&mut reg, &st, st.idle_minutes * 60.0, "", reg_path, in_oc, &mut None);
    let result = (|| -> Result<Option<i32>, ConfigError> {
        let io = |e: std::io::Error| ConfigError(e.to_string());
        match &action {
            Action::Rename { id, name } => {
                let sid = find_session(&reg, id)?;
                set_name(&mut reg, &sid, name); // an empty name gives it its automatic name back
                save_registry(reg_path, &reg).map_err(io)?;
                println!("RENAMED {}", label(&sid, &sessions(&reg)[&sid]));
                return Ok(Some(0));
            }
            Action::Resume { id } => {
                let _ = save_registry(reg_path, &reg);
                let sid = find_session(&reg, id)?;
                let rec = &sessions(&reg)[&sid];
                println!("SESSION {}", label(&sid, rec));
                let cmd = if s(rec.as_object().expect("record"), "source") == Some("opencode") {
                    format!("opencode -s {sid}")
                } else {
                    format!("claude --resume {sid}")
                };
                match rec.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()) {
                    Some(cwd) => println!("cd {} && {cmd}", util::shell_quote(cwd)),
                    None => println!("{cmd}"),
                }
                return Ok(Some(0));
            }
            Action::Load { id } => {
                let _ = save_registry(reg_path, &reg);
                let sid = find_session(&reg, id)?;
                let rec = sessions(&reg)[&sid].clone();
                let file = rec.get("file").and_then(Value::as_str).unwrap_or("").to_string();
                if !fs::metadata(&file).is_ok_and(|m| m.is_file()) {
                    return Err(ConfigError(format!("the transcript of {} is gone", label(&sid, &rec))));
                }
                let text = session_text(&sid, &rec, &read_transcript(&file));
                let written =
                    write_outputs(out_dir, &[(format!("sessions/{sid}"), file.clone(), text.clone())]).map_err(io)?;
                let n = chars(&text);
                println!("SESSION {}", label(&sid, &rec));
                println!("{n:>7} chars  {}", written[0]);
                println!("total: 1 session, {n} chars (~{}k tokens)", n / CHARS_PER_K);
                // one chat, against every chat up to it (last active at or before it)
                let lib = chats_up_to(chat_catalog(&reg, &[]).iter().map(|s| &sessions(&reg)[s]), &last_of(&rec));
                let bytes = fs::metadata(&file).map_or(0, |m| m.len());
                if let Some(note) = usage_of_chats(cfg, cfg_path, &[(n, bytes)], lib) {
                    println!("{note}");
                }
                return Ok(Some(0));
            }
            Action::List { .. } => {
                let _ = save_registry(reg_path, &reg);
            }
        }
        Ok(None)
    })();
    match result {
        Ok(Some(code)) => return code,
        Ok(None) => {}
        Err(e) => {
            println!("SESSION ERROR: {e}");
            return 8;
        }
    }
    let Action::List { limit } = action else { return 0 };
    // --context also registers sessions that were open a moment ago; the list shows closed ones
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let closed = |r: &Value| r.get("mtime").and_then(Value::as_f64).is_none_or(|m| now - m >= st.idle_minutes * 60.0);
    let mut items: Vec<(&String, &Value)> = sessions(&reg).iter().filter(|(_, r)| closed(r)).collect();
    let key = |rec: &Value| -> String {
        ["last", "started"]
            .iter()
            .filter_map(|k| rec.get(*k).and_then(Value::as_str))
            .find(|t| !t.is_empty())
            .unwrap_or("")
            .to_string()
    };
    items.sort_by_key(|(_, r)| std::cmp::Reverse(key(r)));
    println!("{} closed sessions tracked ({added} new); newest first", items.len());
    for (sid, rec) in items.iter().take(limit) {
        println!("{sid}  {:16}  {}", when(rec), rec_name(rec));
    }
    if items.len() > limit {
        println!("... {} more (--limit N)", items.len() - limit);
    }
    0
}

// ---------------------------------------------------------------- --context: earlier chats

/// A preset name: lowercase letters, digits, `.`, `_`, `-` (like a set name).
static PRESET_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9._-]*$").expect("valid"));
/// The preset used when none is named.
pub const DEFAULT_PRESET: &str = "default";

/// What a chat is called in the picker: a name the user gave it, otherwise its first prompt.
fn shown_name(rec: &Value) -> String {
    let first = rec.get("first").and_then(Value::as_str).filter(|f| !f.is_empty());
    match first {
        Some(f) if rec.get("name_source").and_then(Value::as_str) != Some("manual") => f.to_string(),
        _ => rec_name(rec).to_string(),
    }
}

fn chat_label(sid: &str, rec: &Value) -> String {
    format!("\"{}\" ({sid})", shown_name(rec))
}

/// The heading a chat sits under: its project folder (~ for the home folder).
fn chat_folder(rec: &Value) -> String {
    match rec.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()) {
        Some(c) => util::tilde(c),
        None => rec.get("project").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

fn last_of(rec: &Value) -> String {
    ["last", "started"]
        .iter()
        .filter_map(|k| rec.get(*k).and_then(Value::as_str))
        .find(|t| !t.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Every chat that can be loaded, by folder (the folder used most recently first), newest first
/// inside a folder.
fn chat_catalog(reg: &Map<String, Value>, skip: &[&str]) -> Vec<String> {
    /// (folder, its newest last-active time, [(last-active, session id)])
    type Folder = (String, String, Vec<(String, String)>);
    let mut by_folder: Vec<Folder> = Vec::new();
    for (sid, rec) in sessions(reg) {
        let file = rec.get("file").and_then(Value::as_str).unwrap_or("");
        if skip.contains(&sid.as_str()) || !fs::metadata(file).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let (folder, last) = (chat_folder(rec), last_of(rec));
        match by_folder.iter_mut().find(|(f, _, _)| *f == folder) {
            Some((_, newest, v)) => {
                if last > *newest {
                    newest.clone_from(&last);
                }
                v.push((last, sid.clone()));
            }
            None => by_folder.push((folder, last.clone(), vec![(last, sid.clone())])),
        }
    }
    by_folder.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    by_folder
        .into_iter()
        .flat_map(|(_, _, mut v)| {
            v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            v.into_iter().map(|(_, sid)| sid)
        })
        .collect()
}

/// (chats, characters) of the chats last active at or before `cutoff` (all of them when the
/// cutoff is unknown): what loading everything up to that chat would have cost.
fn chats_up_to<'a>(recs: impl Iterator<Item = &'a Value>, cutoff: &str) -> (usize, usize) {
    recs.filter(|r| cutoff.is_empty() || last_of(r).as_str() <= cutoff)
        .fold((0, 0), |(n, c), r| (n + 1, c + chat_chars(r) as usize))
}

fn str_set(rec: Option<&Value>, key: &str) -> BTreeSet<String> {
    rec.and_then(|r| r.get(key))
        .and_then(Value::as_array)
        .map_or_else(BTreeSet::new, |a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

/// One popup section per preset. Like a custom set: nothing ticked on first use, chats added
/// since the preset was saved show NEW but are never ticked on their own, and Remember is only
/// pre-ticked on first use (new chats come all the time; a one-off load must not replace the
/// preset).
fn chat_sections(names: &[String], catalog: &[String], presets: &Choices, setup: bool) -> Vec<Section> {
    names
        .iter()
        .map(|n| {
            let rec = presets.get(n);
            let selected = str_set(rec, "selected");
            let known = str_set(rec, "known");
            let new: BTreeSet<String> = match rec {
                Some(_) => catalog.iter().filter(|d| !known.contains(*d)).cloned().collect(),
                None => BTreeSet::new(),
            };
            let main: Vec<String> = catalog.iter().filter(|d| selected.contains(*d)).cloned().collect();
            let other: Vec<String> = catalog.iter().filter(|d| !selected.contains(*d)).cloned().collect();
            Section {
                name: n.clone(),
                kind: SetKind::Custom,
                docs: catalog.to_vec(),
                ticked: main.iter().cloned().collect(),
                main,
                other,
                new,
                remember: rec.is_none() || setup,
                first: rec.is_none(),
            }
        })
        .collect()
}

pub struct ContextArgs<'a> {
    pub names: &'a [String],
    pub setup: bool,
    pub list: bool,
    pub reg_path: &'a str,
    pub presets_path: &'a str,
    pub out_dir: &'a str,
    pub cfg_path: &'a str,
    pub ui_path: &'a str,
}

fn chat_chars(rec: &Value) -> u64 {
    rec.get("chars").and_then(Value::as_u64).unwrap_or(0)
}

/// Log a chat load (and send it when sharing is on). Returns the sharing note, if any.
fn usage_of_chats(cfg: &Config, cfg_path: &str, loaded: &[(usize, u64)], lib: (usize, usize)) -> Option<&'static str> {
    if loaded.is_empty() || !crate::usage::enabled(cfg) {
        return None;
    }
    let rec = crate::usage::chat_record(loaded, lib).ok()?;
    crate::usage::finish(cfg, cfg_path, &rec)
}

/// The chats side of a load, ready for the popup.
pub struct Chats {
    presets: Choices,
    names: Vec<String>,
    reg: Map<String, Value>,
    catalog: Vec<String>,
    pub secs: Vec<Section>,
    /// The other tab of a load that started on research: quiet, and only loaded when ticked.
    secondary: bool,
}

/// Find the chats for `--context [PRESET ...]` (or, as the second tab, the preset `default`).
/// Err(code) = stop with that exit code (the message is printed, unless secondary).
pub fn prep(cfg: &Config, a: &ContextArgs, secondary: bool) -> Result<Chats, i32> {
    let say = |m: String| {
        if !secondary {
            println!("{m}");
        }
    };
    let Some(st) = settings(cfg) else {
        say("SESSIONS OFF: add `[sessions]` with `enabled = true` to the config to load earlier chats; nothing done"
            .into());
        return Err(7);
    };
    let presets = library::load_choices(a.presets_path);
    if a.list {
        if presets.is_empty() {
            println!("no context presets saved yet");
        }
        for (n, rec) in &presets {
            let k = str_set(Some(rec), "selected").len();
            println!("{n:20} {k:>4} chats");
        }
        return Err(0);
    }
    let names: Vec<String> = if a.names.is_empty() {
        vec![DEFAULT_PRESET.to_string()]
    } else {
        library::dedupe(a.names.iter().map(|n| n.trim().to_lowercase()))
    };
    let bad: Vec<&str> = names.iter().filter(|n| !PRESET_RE.is_match(n)).map(String::as_str).collect();
    if !bad.is_empty() {
        say(format!("bad preset name(s): {} (use lowercase letters, digits, . _ -)", bad.join(", ")));
        return Err(2);
    }

    // the chat that runs this is never offered; chats closed a moment ago are
    let me = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default().to_lowercase();
    let mut reg = load_registry(a.reg_path);
    let mut live = None; // inside opencode: the opencode chat that runs this
    scan_sessions(&mut reg, &st, 0.0, &me, a.reg_path, std::env::var_os("OPENCODE").is_some(), &mut live);
    if let Err(e) = save_registry(a.reg_path, &reg) {
        say(format!("could not save the session registry to {}: {e}", a.reg_path));
    }
    let catalog = chat_catalog(&reg, &[&me, live.as_deref().unwrap_or("")]);
    if catalog.is_empty() {
        if st.opencode {
            say(format!("NO CHATS: no earlier Claude Code ({}) or opencode chats; nothing loaded", st.projects_dir));
        } else {
            say(format!("NO CHATS: no earlier Claude Code chats in {}; nothing loaded", st.projects_dir));
        }
        return Err(0);
    }
    let mut secs = chat_sections(&names, &catalog, &presets, a.setup);
    if secondary {
        for s in &mut secs {
            s.remember = false; // a second tab never saves a preset unless the user asks
        }
    }
    Ok(Chats { presets, names, reg, catalog, secs, secondary })
}

/// The popup tab of the chats.
pub fn tab(c: &Chats, ui_path: &str) -> popup::Tab {
    let (chip, by_time) = crate::state::ui(ui_path, true);
    let mut look = popup::Look { chats: true, chip, by_time, ..popup::Look::default() };
    look.taken = c.presets.keys().cloned().collect();
    let map = sessions(&c.reg);
    let mut sizes = popup::Sizes::new();
    for sid in &c.catalog {
        let rec = &map[sid];
        look.labels.insert(sid.clone(), shown_name(rec));
        if let Some(first) = rec.get("first").and_then(Value::as_str).filter(|f| !f.is_empty()) {
            look.defaults.insert(sid.clone(), first.to_string());
        }
        look.groups.insert(sid.clone(), chat_folder(rec));
        let when = crate::bands::parse_iso(&last_of(rec))
            .or_else(|| rec.get("mtime").and_then(Value::as_f64).map(|m| m as i64));
        if let Some(w) = when {
            look.times.insert(sid.clone(), w);
            // the local day, like the Today / Yesterday headings (the record holds UTC)
            look.notes.insert(sid.clone(), crate::bands::local(w).format("%Y-%m-%d").to_string());
        }
        sizes.insert(sid.clone(), chat_chars(rec) as usize);
    }
    popup::Tab { sections: c.secs.clone(), sizes, look }
}

/// Give a chat its automatic name back (its title or first prompt, as a rescan would).
fn reset_name(reg: &mut Map<String, Value>, sid: &str) {
    let Some(rec) = sessions_mut(reg).get_mut(sid).and_then(Value::as_object_mut) else { return };
    rec.insert("name_source".into(), "auto".into());
    let get = |k: &str| rec.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let (file, proj, source) = (get("file"), get("project"), get("source"));
    let mtime = rec.get("mtime").and_then(Value::as_f64).unwrap_or(0.0);
    let entries = read_transcript(&file);
    if entries.is_empty() {
        // the transcript is gone: nothing to read a title from, so the first prompt it had
        let first = clean_name(&get("first"));
        if !first.is_empty() {
            rec.insert("name".into(), Value::String(first));
        }
        return;
    }
    register(reg, sid, &file, mtime, &proj, Some(source.as_str()).filter(|s| !s.is_empty()), &entries);
}

/// Name a chat (an empty name gives it its automatic name back).
fn set_name(reg: &mut Map<String, Value>, sid: &str, name: &str) {
    let name = clean_name(name);
    if name.is_empty() {
        reset_name(reg, sid);
        return;
    }
    if let Some(rec) = sessions_mut(reg).get_mut(sid).and_then(Value::as_object_mut) {
        rec.insert("name".into(), Value::String(name));
        rec.insert("name_source".into(), "manual".into());
    }
}

/// Put the remembered picks into the presets (a new name moves the ticks: the old name goes).
/// Returns true when something changed.
fn save_presets(presets: &mut Choices, picks: &[popup::Pick], catalog: &[String], setup: bool) -> bool {
    let mut saved_any = false;
    for p in picks.iter().filter(|p| p.remember || setup) {
        let selected: Vec<&String> = catalog.iter().filter(|d| p.ticked.contains(*d)).collect();
        let to = if p.save_as.is_empty() { p.name.clone() } else { p.save_as.clone() };
        if to != p.name && presets.remove(&p.name).is_some() {
            println!("RENAMED context preset {} to {to}", p.name);
        }
        presets.insert(to.clone(), json!({"selected": selected, "known": catalog}));
        println!("SAVED context preset {to}: {} of {} chats", p.ticked.len(), catalog.len());
        saved_any = true;
    }
    saved_any
}

/// Save and load what was picked (`picks` None: no popup, the saved presets). Returns the exit
/// code.
pub fn finish(c: Chats, cfg: &Config, a: &ContextArgs, picks: Option<popup::TabPick>) -> i32 {
    let Chats { mut presets, names, mut reg, catalog, secs, secondary } = c;
    let unsaved: Vec<&str> = names.iter().filter(|n| !presets.contains_key(*n)).map(String::as_str).collect();
    if picks.is_none() && !unsaved.is_empty() {
        if !secondary {
            println!("NO POPUP: context preset {} has no saved choice yet; nothing loaded", unsaved.join(", "));
            return 5;
        }
        return 0;
    }
    // a saved preset can name a chat whose transcript was deleted since (the popup just drops it;
    // a profile preset passes it on)
    let all: BTreeSet<&String> = catalog.iter().collect();
    let saved: Vec<String> = match &picks {
        None => names.iter().flat_map(|n| str_set(presets.get(n), "selected")).collect(),
        Some(t) => t.picks.iter().flat_map(|p| p.ticked.iter().cloned()).collect(),
    };
    // the running chat (and the live opencode one) is left out of the catalog, but not gone
    let on_disk = |d: &String| {
        sessions(&reg)
            .get(d)
            .and_then(|r| r.get("file"))
            .and_then(Value::as_str)
            .is_some_and(|f| std::path::Path::new(f).is_file())
    };
    let gone = library::dedupe(saved.into_iter().filter(|d| !all.contains(d) && !on_disk(d)));
    let picks = match picks {
        Some(t) => {
            if let Err(e) = crate::state::save_ui(a.ui_path, true, t.chip, t.by_time) {
                println!("could not save the window settings to {}: {e}", a.ui_path);
            }
            if !t.renames.is_empty() {
                // renamed in the file as it is now: a rename another run saved meanwhile stays
                let mut fresh = load_registry(a.reg_path);
                for (sid, name) in &t.renames {
                    set_name(&mut reg, sid, name);
                    set_name(&mut fresh, sid, name);
                    println!("RENAMED {}", chat_label(sid, &sessions(&reg)[sid]));
                }
                if let Err(e) = save_registry(a.reg_path, &fresh) {
                    println!("could not save the session registry to {}: {e}", a.reg_path);
                }
            }
            t.picks
        }
        None => secs
            .iter()
            .map(|s| popup::Pick {
                name: s.name.clone(),
                ticked: s.ticked.clone(),
                remember: false,
                save_as: s.name.clone(),
                title: None,
            })
            .collect(),
    };
    if secondary && picks.iter().all(|p| p.ticked.is_empty()) {
        return 0; // the other tab, left alone
    }

    if picks.iter().any(|p| p.remember || a.setup) {
        presets = library::load_choices(a.presets_path); // a preset another run saved meanwhile stays
    }
    let saved_any = save_presets(&mut presets, &picks, &catalog, a.setup);
    if saved_any && let Err(e) = library::save_choices(a.presets_path, &presets) {
        println!("could not save the presets to {}: {e}", a.presets_path);
    }
    if a.setup {
        println!("setup only: nothing loaded");
        return 0;
    }

    let map = sessions(&reg);
    let chosen: BTreeSet<&String> = picks.iter().flat_map(|p| p.ticked.iter()).collect();
    let mut docs = Vec::new();
    let mut labels = Vec::new();
    for sid in catalog.iter().filter(|d| chosen.contains(d)) {
        let rec = &map[sid];
        let file = rec.get("file").and_then(Value::as_str).unwrap_or("").to_string();
        let text = session_text(sid, rec, &read_transcript(&file));
        labels.push(chat_label(sid, rec));
        docs.push((format!("sessions/{sid}"), file, text));
    }
    let written = match write_outputs(a.out_dir, &docs) {
        Ok(w) => w,
        Err(e) => {
            println!("SESSION ERROR: could not write to {}: {e}", a.out_dir);
            return 8;
        }
    };
    let mut total = 0;
    let mut loaded = Vec::new();
    for ((label, (_, file, text)), path) in labels.iter().zip(&docs).zip(&written) {
        let n = chars(text);
        total += n;
        loaded.push((n, fs::metadata(file).map_or(0, |m| m.len())));
        println!("CHAT {label}");
        println!("{n:>7} chars  {path}");
    }
    for sid in gone {
        println!("MISSING  chat {sid} (its transcript is gone)");
    }
    println!("total: {} chats, {total} chars (~{}k tokens)", docs.len(), total / CHARS_PER_K);
    // the "whole library" of this load: every chat up to the newest one picked
    let newest = chosen.iter().filter_map(|sid| map.get(*sid)).map(last_of).max().unwrap_or_default();
    let lib = chats_up_to(catalog.iter().map(|sid| &map[sid]), &newest);
    if let Some(note) = usage_of_chats(cfg, a.cfg_path, &loaded, lib) {
        println!("{note}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pastes_are_prompts_and_compaction_summaries_are_not() {
        let e = |v: Value| v.as_object().unwrap().clone();
        let paste = e(json!({"type": "user", "message": {"content":
            "\n\n<pasted_content id=\"ab12\">\nthe brief\n</pasted_content id=\"ab12\">\n"}}));
        assert_eq!(real_prompt(&paste), "the brief");
        let summary = e(json!({"type": "user", "isCompactSummary": true, "message": {"content": "This session is"}}));
        assert_eq!(real_prompt(&summary), "");
        assert_eq!(real_prompt(&e(json!({"type": "user", "message": {"content": "<command-name>x"}}))), "");
    }

    #[test]
    fn names_are_cleaned_and_cut() {
        assert_eq!(clean_name("  a \n  b  "), "a b");
        let long = "x".repeat(100);
        let c = clean_name(&long);
        assert_eq!(chars(&c), NAME_MAX);
        assert!(c.ends_with('…'));
    }

    #[test]
    fn a_renamed_preset_moves_its_ticks() {
        let mut presets: Choices =
            serde_json::from_str(r#"{"old": {"selected": ["a"], "known": ["a"]}, "keep": {}}"#).unwrap();
        let pick = |name: &str, save_as: &str, remember: bool| popup::Pick {
            name: name.into(),
            ticked: ["b".to_string()].into(),
            remember,
            save_as: save_as.into(),
            title: None,
        };
        let catalog = ["a".to_string(), "b".to_string()];
        assert!(!save_presets(&mut presets, &[pick("old", "new", false)], &catalog, false)); // not remembered
        assert!(presets.contains_key("old"));
        assert!(save_presets(&mut presets, &[pick("old", "new", true)], &catalog, false));
        assert!(!presets.contains_key("old"));
        assert_eq!(presets["new"]["selected"], serde_json::json!(["b"]));
        assert!(presets.contains_key("keep"));
        assert!(save_presets(&mut presets, &[pick("fresh", "fresh", true)], &catalog, false));
        assert_eq!(presets["fresh"]["known"], serde_json::json!(["a", "b"]));
    }
}
