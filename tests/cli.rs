// SPDX-License-Identifier: MIT
//! End-to-end tests of the `researcher` binary, ported from tests/test_researcher.py.
//!
//! Every run sets RESEARCHER_NO_POPUP=1 (tests must never open a window) and points all XDG
//! folders into a temporary directory (the usage log must never reach a real home).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_researcher");
const SID1: &str = "11111111-1111-4111-8111-111111111111";
const SID2: &str = "22222222-2222-4222-8222-222222222222";
const SID3: &str = "33333333-3333-4333-8333-333333333333";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

struct Env {
    tmp: PathBuf,
    lib: PathBuf,
    conf_dir: PathBuf,
    out: PathBuf,
    data: PathBuf,
    cfg: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.tmp);
    }
}

impl Env {
    fn new() -> Env {
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
        let tmp = std::env::temp_dir().join(format!(
            "researcher-test-{}-{}-{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let lib = tmp.join("lib");
        write(&lib.join("rust/linux.html"), "<h1>Linux</h1><p>one</p><script>x()</script>");
        write(&lib.join("rust/memory-a.md"), "# A");
        write(&lib.join("rust/memory-b.txt"), "B");
        write(&lib.join("web/seo.md"), "seo");
        write(&lib.join("app/roadmap/index.html"), "<p>road</p>");
        write(&lib.join(".hidden/x.md"), "no");
        write(&lib.join("rust/notes.png"), "no");
        let conf_dir = tmp.join("conf");
        let cfg = conf_dir.join("config.toml");
        write(
            &cfg,
            &format!(
                "library = \"{}\"\ndefault_set = \"rust\"\n[aliases]\nrs = \"rust\"\n[sets.rust]\n\
                 docs = [\"rust/linux\", \"rust/memory-*\"]\n[sets.app]\ninclude = [\"rust\"]\n\
                 docs = [\"app/roadmap\", \"app/ghost\"]\n",
                lib.display()
            ),
        );
        Env { out: tmp.join("out"), data: tmp.join("data"), lib, conf_dir, cfg, tmp }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env("RESEARCHER_NO_POPUP", "1")
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_CONFIG_HOME", self.tmp.join("xdg-config"))
            .env("XDG_CACHE_HOME", self.tmp.join("xdg-cache"))
            .env_remove("RESEARCHER_LIBRARY")
            .env_remove("CLAUDE_CODE_SESSION_ID") // the tests may run inside Claude Code
            .env_remove("OPENCODE"); // or inside opencode
        c
    }

    /// (exit code, stdout) of `researcher --config CFG --out OUT ARGS...`
    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_with(args, &[])
    }

    /// `run` with extra environment variables.
    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
        let mut c = self.cmd();
        c.envs(env.iter().copied());
        let o = c.arg("--config").arg(&self.cfg).arg("--out").arg(&self.out).args(args).output().unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn usage_file(&self) -> PathBuf {
        self.data.join("researcher/usage.jsonl")
    }

    fn records(&self) -> Vec<Value> {
        read(&self.usage_file()).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn out_txts(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.out)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|f| f.ends_with(".txt"))
            .collect();
        v.sort();
        v
    }

    fn append_cfg(&self, text: &str) {
        write(&self.cfg, &(read(&self.cfg) + text));
    }
}

// ---------------------------------------------------------------- usage log

#[test]
fn load_logs_counts_only() {
    let e = Env::new();
    e.run(&["--no-popup", "rust"]);
    let recs = e.records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert_eq!(r["docs"], 3);
    assert_eq!(r["v"], 3);
    assert_eq!(r["fmt"], serde_json::json!({"pdf": 0, "html": 1, "md": 1, "txt": 1, "chat": 0}));
    // no [usage] before: every document counts against the format it was loaded from
    assert_eq!(r["vs_chars"], r["fmt_chars"]);
    // the loaded characters per format add up to the loaded characters
    let fc = r["fmt_chars"].as_object().unwrap();
    assert_eq!(fc.values().map(|v| v.as_u64().unwrap()).sum::<u64>(), r["chars"].as_u64().unwrap());
    assert_eq!(fc["chat"], 0);
    assert!(fc["html"].as_u64() > Some(0) && fc["md"].as_u64() > Some(0) && fc["pdf"] == 0);
    assert_eq!(r["lib_docs"], 5);
    assert!(r["lib_chars"].as_u64() > r["chars"].as_u64());
    assert!(r["raw"].as_u64() >= r["chars"].as_u64());
    let id = r["id"].as_str().unwrap();
    assert!(id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_eq!(r["tk"], 2400);
}

#[test]
fn log_holds_no_names_paths_or_titles() {
    let e = Env::new();
    e.run(&["--no-popup", "rust"]);
    let raw = read(&e.usage_file()) + &read(&e.data.join("researcher/usage-id"));
    for word in ["rust", "linux", "memory", "roadmap", "seo", "ludalunizz"] {
        assert!(!raw.contains(word), "{word} in the log");
    }
    assert!(!raw.contains(&e.lib.display().to_string()));
    assert!(!raw.contains(&e.tmp.display().to_string()));
}

#[test]
fn same_install_id_every_load() {
    let e = Env::new();
    e.run(&["--no-popup", "rust"]);
    e.run(&["--no-popup", "rs"]);
    let r = e.records();
    assert_eq!(r[0]["id"], r[1]["id"]);
}

#[test]
fn log_can_be_switched_off() {
    let e = Env::new();
    e.append_cfg("\n[usage]\nlog = false\n");
    assert_eq!(e.run(&["--no-popup", "rust"]).0, 0);
    assert!(!e.usage_file().exists());
    assert!(!e.data.join("researcher/usage-id").exists());
}

#[test]
fn setup_sync_list_and_nothing_loaded_do_not_log() {
    let e = Env::new();
    e.run(&["--sync"]);
    e.run(&["--list"]);
    e.run(&["--no-popup", "newproj"]); // custom set, nothing chosen
    assert!(!e.usage_file().exists());
}

#[test]
fn unwritable_log_does_not_break_a_load() {
    let e = Env::new();
    write(&e.data.join("researcher"), "a file where the folder should be");
    assert_eq!(e.run(&["--no-popup", "rust"]).0, 0);
}

#[test]
fn usage_report_and_export() {
    let e = Env::new();
    e.run(&["--no-popup", "rust"]);
    let (code, out) = e.run(&["--usage"]);
    assert_eq!(code, 0);
    assert!(out.contains("1 loads"), "{out}");
    assert!(out.contains("squeezed away"));
    let dst = e.tmp.join("export.json");
    let (_, out) = e.run(&["--usage-export", dst.to_str().unwrap()]);
    assert!(out.contains("Nothing was sent"));
    let data: Value = serde_json::from_str(&read(&dst)).unwrap();
    assert_eq!(data["schema"], 1);
    assert_eq!(data["loads"].as_array().unwrap().len(), 1);
}

#[test]
fn usage_with_no_log() {
    let e = Env::new();
    assert!(e.run(&["--usage"]).1.contains("no usage recorded yet"));
}

#[test]
fn bad_usage_lines_skipped() {
    let e = Env::new();
    write(&e.usage_file(), "not json\n{\"v\": 99}\n{\"v\": 1, \"docs\": 1, \"day\": \"2026-11-16\"}\n");
    assert!(e.run(&["--usage"]).1.contains("1 loads on 1 days"));
}

// ---------------------------------------------------------------- sharing (opt-in sending)

/// A one-off local receiver: answers each request with the next (status, body) and hands the
/// request bodies back. Tests never reach the real receiver.
fn receiver(replies: Vec<(u16, &'static str)>) -> (String, std::sync::mpsc::Receiver<Value>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/usage", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in replies {
            let Ok((mut s, _)) = listener.accept() else { return };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut buf = vec![0; len];
            r.read_exact(&mut buf).unwrap();
            let _ = tx.send(serde_json::from_slice(&buf).unwrap_or(Value::Null));
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(reply.as_bytes()).unwrap();
        }
    });
    (url, rx)
}

const REC: &str = r#"{"chars":5,"day":"2026-11-16","docs":1,"fmt":{"html":0,"md":1,"pdf":0,"txt":0},"id":"0123456789abcdef","lib_chars":9,"lib_docs":2,"raw":5,"src_bytes":5,"tk":2400,"v":1}"#;

impl Env {
    fn share_on(&self, url: &str) {
        self.append_cfg(&format!("\n[usage]\nshare = true\nurl = \"{url}\"\n"));
    }

    fn log_lines(&self, n: usize) {
        write(&self.usage_file(), &format!("{REC}\n").repeat(n));
    }

    fn sent_marker(&self) -> Option<String> {
        fs::read_to_string(self.data.join("researcher/usage-sent")).ok()
    }
}

fn seqs(body: &Value) -> Vec<u64> {
    body["loads"].as_array().unwrap().iter().map(|r| r["seq"].as_u64().unwrap()).collect()
}

#[test]
fn nothing_is_sent_until_the_user_says_yes() {
    let e = Env::new();
    let (code, out) = e.run(&["--no-popup", "rust"]);
    assert_eq!(code, 0);
    assert!(out.trim_end().ends_with("USAGE SHARING: not decided (researcher --usage-share on|off)"), "{out}");
    let (code, out) = e.run(&["--usage-send"]);
    assert_eq!(code, 9, "{out}");
    assert!(out.contains("not decided; nothing sent"), "{out}");
    assert!(e.run(&["--usage"]).1.contains("sharing: not decided, nothing is sent"));
    assert!(e.sent_marker().is_none());

    e.run(&["--usage-share", "off"]);
    let (code, out) = e.run(&["--no-popup", "rust"]);
    assert_eq!(code, 0);
    assert!(!out.contains("USAGE SHARING"), "{out}");
    assert_eq!(e.run(&["--usage-send"]).0, 9);
}

#[test]
fn usage_share_writes_one_line_and_keeps_the_rest() {
    let e = Env::new();
    e.append_cfg("# my note\n[usage] # counts\nlog = true\n[sets.web]\ndocs = [\"web/*\"]\n");
    let (code, out) = e.run(&["--usage-share", "on"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("USAGE SHARING: on"), "{out}");
    let body = read(&e.cfg);
    assert!(body.contains("# my note\n[usage] # counts\nshare = true\nlog = true\n[sets.web]"), "{body}");
    e.run(&["--usage-share", "off"]);
    let body = read(&e.cfg);
    assert_eq!(body.matches("share =").count(), 1, "{body}");
    assert!(body.contains("share = false\nlog = true\n"), "{body}");
    assert_eq!(e.run(&["--list"]).0, 0); // the config still reads, sets and all

    let fresh = e.tmp.join("none/config.toml");
    let o = e.cmd().arg("--config").arg(&fresh).args(["--usage-share", "on"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(read(&fresh), "[usage]\nshare = true\n");
}

#[test]
fn dry_run_prints_the_exact_request_and_sends_nothing() {
    let e = Env::new();
    e.run(&["--no-popup", "rust"]);
    e.run(&["--no-popup", "rust"]); // the same load twice: two records with their own seq
    let (code, out) = e.run(&["--usage-send", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    let (first, json) = out.split_once('\n').unwrap();
    assert!(first.starts_with("DRY RUN: 2 records would be sent to https://"), "{first}");
    assert!(first.contains("(sharing: not decided); nothing was sent"), "{first}");
    let body: Value = serde_json::from_str(json).unwrap();
    assert_eq!(body["schema"], 2);
    assert_eq!(seqs(&body), [1, 2]);
    let mut a = body["loads"][0].clone();
    a["seq"] = Value::Null;
    let mut b = body["loads"][1].clone();
    b["seq"] = Value::Null;
    assert_eq!(a, b);
    assert!(e.sent_marker().is_none());
}

#[test]
fn send_posts_only_what_is_new() {
    let e = Env::new();
    let ok = r#"{"ok":true,"received":3,"new":3}"#;
    let (url, got) = receiver(vec![(200, ok), (200, r#"{"ok":true,"received":1,"new":1}"#)]);
    e.share_on(&url);
    e.log_lines(3);
    let (code, out) = e.run(&["--usage-send"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with(&format!("SENT: 3 records to {url} (3 new there)")), "{out}");
    let body = got.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(body["schema"], 2);
    assert_eq!(seqs(&body), [1, 2, 3]);
    assert_eq!(e.sent_marker().as_deref(), Some("3\n"));

    let (code, out) = e.run(&["--usage-send"]);
    assert_eq!((code, out.trim()), (0, "NOTHING TO SEND: every usage record was sent already"));
    write(&e.usage_file(), &(read(&e.usage_file()) + "not json\n" + REC + "\n"));
    assert!(e.run(&["--usage"]).1.contains("sharing: on, 1 records waiting"));
    assert_eq!(e.run(&["--usage-send"]).0, 0);
    assert_eq!(seqs(&got.recv_timeout(Duration::from_secs(5)).unwrap()), [5]);
    assert_eq!(e.sent_marker().as_deref(), Some("5\n"));
}

#[test]
fn a_refused_record_is_skipped_not_stuck() {
    let e = Env::new();
    let (url, got) = receiver(vec![
        (400, r#"{"ok":false,"error":"record 1: bad day"}"#),
        (200, r#"{"ok":true,"received":2,"new":2}"#),
    ]);
    e.share_on(&url);
    e.log_lines(3);
    let (code, out) = e.run(&["--usage-send"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("SENT: 2 records") && out.contains("1 refused by the receiver and skipped"), "{out}");
    assert_eq!(seqs(&got.recv_timeout(Duration::from_secs(5)).unwrap()), [1, 2, 3]);
    assert_eq!(seqs(&got.recv_timeout(Duration::from_secs(5)).unwrap()), [1, 3]);
    assert_eq!(e.sent_marker().as_deref(), Some("3\n"));
}

#[test]
fn a_failed_send_keeps_the_queue() {
    let e = Env::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    e.share_on(&format!("http://127.0.0.1:{port}/v1/usage")); // nothing listens there
    e.log_lines(2);
    let (code, out) = e.run(&["--usage-send"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.starts_with("SEND FAILED: ") && out.contains("0 records sent before that, 2 stay queued"), "{out}");
    assert!(e.sent_marker().is_none());

    let (url, _got) = receiver(vec![(500, "oops")]);
    write(
        &e.cfg,
        &read(&e.cfg)
            .replace(&format!("127.0.0.1:{port}"), url.trim_start_matches("http://").trim_end_matches("/v1/usage")),
    );
    let (code, out) = e.run(&["--usage-send"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("HTTP 500: oops"), "{out}");
    assert!(e.sent_marker().is_none());
}

#[test]
fn a_cut_log_starts_counting_again() {
    let e = Env::new();
    e.log_lines(2);
    write(&e.data.join("researcher/usage-sent"), "10\n");
    let (_, out) = e.run(&["--usage-send", "--dry-run"]);
    assert!(out.starts_with("DRY RUN: 2 records"), "{out}");
}

#[test]
fn a_load_sends_in_the_background_when_sharing_is_on() {
    let e = Env::new();
    let (url, got) = receiver(vec![(200, r#"{"ok":true,"received":1,"new":1}"#)]);
    e.share_on(&url);
    let (code, out) = e.run(&["--no-popup", "rust"]);
    assert_eq!(code, 0);
    assert!(!out.contains("USAGE SHARING"), "{out}");
    let body = got.recv_timeout(Duration::from_secs(20)).expect("the background send arrives");
    assert_eq!(seqs(&body), [1]);
    assert_eq!(body["loads"][0]["docs"], 3);
}

// ---------------------------------------------------------------- library, sets, main flow

#[test]
fn scan_ids_and_hidden_and_other_files() {
    let e = Env::new();
    let (code, out) = e.run(&["--sync"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("text copy: 5 of 5 docs"), "{out}");
    assert_eq!(
        e.out_txts(),
        ["app - roadmap.txt", "rust - linux.txt", "rust - memory-a.txt", "rust - memory-b.txt", "web - seo.txt"]
    );
}

#[test]
fn exclude_globs() {
    let e = Env::new();
    e.append_cfg(""); // keep the file, add exclude at the top
    write(&e.cfg, &format!("exclude = [\"web/*\", \"app/*\"]\n{}", read(&e.cfg)));
    let (_, out) = e.run(&["--sync"]);
    assert!(out.contains("text copy: 3 of 3 docs"), "{out}");
}

#[test]
fn load_counts_against_the_format_read_before() {
    let e = Env::new();
    write(&e.lib.join("rust/linux.pdf"), "%PDF"); // the HTML report also exists as PDF
    write(&e.lib.join("rust/memory-a.pdf"), "%PDF"); // and the markdown one
    let (code, out) = e.run(&["--usage-before", "pdf"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("USAGE BEFORE: pdf"), "{out}");
    assert!(read(&e.cfg).contains("[usage]\nbefore = \"pdf\"\n"), "{}", read(&e.cfg));
    e.run(&["--no-popup", "rust"]);
    let r = &e.records()[0];
    let (fc, vs) = (&r["fmt_chars"], &r["vs_chars"]);
    assert_eq!(vs["pdf"].as_u64().unwrap(), fc["html"].as_u64().unwrap() + fc["md"].as_u64().unwrap());
    assert_eq!(vs["html"], 0);
    assert_eq!(vs["md"], 0);
    assert_eq!(vs["txt"], fc["txt"]); // memory-b has no PDF: the one copy it has
    // set back: the loaded formats again; the line is replaced, not added
    e.run(&["--usage-before", "none"]);
    assert_eq!(read(&e.cfg).matches("before =").count(), 1);
    e.run(&["--no-popup", "rust"]);
    let r = &e.records()[1];
    assert_eq!(r["vs_chars"], r["fmt_chars"]);
    let (code, _) = e.run(&["--usage-before", "md"]);
    assert_ne!(code, 0);
}

#[test]
fn html_preferred_over_pdf() {
    let e = Env::new();
    write(&e.lib.join("web/seo.html"), "<p>h</p>");
    write(&e.lib.join("web/seo.pdf"), "%PDF");
    e.run(&["--sync"]);
    assert!(
        read(&e.out.join("web - seo.txt")).starts_with(&format!("SOURCE: {}", e.lib.join("web/seo.html").display()))
    );
}

#[test]
fn no_popup_first_use_loads_everything_in_set() {
    let e = Env::new();
    let (code, out) = e.run(&["--no-popup", "rust"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("total: 3 docs"), "{out}");
    // the usage log sizes the whole library, so every document gets its text copy
    assert_eq!(
        e.out_txts(),
        ["app - roadmap.txt", "rust - linux.txt", "rust - memory-a.txt", "rust - memory-b.txt", "web - seo.txt"]
    );
    let body = read(&e.out.join("rust - linux.txt"));
    assert!(body.contains("Linux"));
    assert!(!body.contains("x()"));
}

#[test]
fn printed_paths_are_the_copies() {
    let e = Env::new();
    let (_, out) = e.run(&["--no-popup", "rust"]);
    let paths: Vec<&str> = out.lines().filter_map(|l| l.split_once(" chars  ").map(|(_, p)| p)).collect();
    assert_eq!(paths.len(), 3);
    for p in paths {
        assert!(Path::new(p).is_file(), "{p}");
    }
}

#[test]
fn alias_and_default_set() {
    let e = Env::new();
    assert_eq!(e.run(&["--no-popup", "rs"]).0, 0);
    assert_eq!(e.run(&["--no-popup"]).0, 0);
}

#[test]
fn missing_doc_reported() {
    let e = Env::new();
    let (code, out) = e.run(&["--no-popup", "app"]);
    assert_eq!(code, 0);
    assert!(out.contains("MISSING  app/ghost"), "{out}");
    assert!(out.contains("total: 4 docs"), "{out}");
}

#[test]
fn env_switch_blocks_popup() {
    let e = Env::new();
    let (code, out) = e.run(&["rust"]); // no --no-popup flag, env switch only
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("NO POPUP ("));
}

#[test]
fn custom_without_choice_loads_nothing() {
    let e = Env::new();
    let (code, out) = e.run(&["--no-popup", "newproj"]);
    assert_eq!(code, 5);
    assert!(out.contains("nothing loaded"));
}

#[test]
fn custom_set_with_saved_choice() {
    let e = Env::new();
    write(
        &e.conf_dir.join("choices.json"),
        r#"{"side": {"custom": true, "selected": ["web/seo"], "known": ["app/roadmap", "rust/linux", "rust/memory-a", "rust/memory-b"]}}"#,
    );
    let (code, out) = e.run(&["--no-popup", "side"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("total: 1 docs"), "{out}"); // web/seo is NEW but was chosen before
    let (_, out) = e.run(&["--list"]);
    assert!(out.contains("(custom, saved default 1)"), "{out}");
}

#[test]
fn defined_set_new_docs_ticked() {
    let e = Env::new();
    write(
        &e.conf_dir.join("choices.json"),
        r#"{"rust": {"selected": ["rust/linux"], "known": ["rust/linux", "rust/memory-a"]}}"#,
    );
    let (_, out) = e.run(&["--no-popup", "rust"]);
    assert!(out.contains("total: 2 docs"), "{out}");
    assert!(out.contains("left out by choice (1): rust/memory-a"), "{out}");
}

#[test]
fn unknown_set_exit_2() {
    let e = Env::new();
    assert_eq!(e.run(&["--no-popup", "Not A Name!"]).0, 2);
}

#[test]
fn no_library() {
    let e = Env::new();
    write(&e.cfg, "");
    let (code, out) = e.run(&["--no-popup", "x"]);
    assert_eq!(code, 6);
    assert!(out.contains("NO LIBRARY"));
}

#[test]
fn bad_toml() {
    let e = Env::new();
    write(&e.cfg, "library = [");
    let (code, out) = e.run(&["--no-popup", "x"]);
    assert_eq!(code, 6);
    assert!(out.contains("CONFIG ERROR"));
}

#[test]
fn include_loop_is_a_config_error() {
    let e = Env::new();
    e.append_cfg("[sets.a]\ninclude = [\"b\"]\n[sets.b]\ninclude = [\"a\"]\n");
    let (code, out) = e.run(&["--no-popup", "rust"]);
    assert_eq!(code, 6);
    assert!(out.contains("loop"), "{out}");
}

#[test]
fn text_copies_persist_and_foreign_files_kept() {
    let e = Env::new();
    e.run(&["--no-popup", "rust"]);
    let keep = e.out.join("mine.txt");
    write(&keep, "mine");
    write(&e.cfg, &read(&e.cfg).replace(r#"docs = ["rust/linux", "rust/memory-*"]"#, r#"docs = ["rust/linux"]"#));
    let (_, out) = e.run(&["--no-popup", "rust"]);
    assert!(out.contains("total: 1 docs"), "{out}");
    let files = e.out_txts();
    assert!(files.contains(&"mine.txt".to_string()));
    assert!(files.contains(&"rust - memory-a.txt".to_string())); // not loaded this time, but kept
    assert_eq!(read(&keep), "mine");
}

fn snapshot(dir: &Path) -> Vec<(PathBuf, String, SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((p.clone(), read(&p), fs::metadata(&p).unwrap().modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn library_is_never_changed() {
    let e = Env::new();
    let before = snapshot(&e.lib);
    e.run(&["--no-popup", "rust"]);
    e.run(&["--sync"]);
    assert_eq!(before, snapshot(&e.lib));
}

#[test]
fn copy_reused_until_source_changes() {
    let e = Env::new();
    e.run(&["--no-popup", "rust"]);
    let copy = e.out.join("rust - memory-a.txt");
    let head = read(&copy).lines().next().unwrap().to_string(); // the SOURCE line must stay
    write(&copy, &format!("{head}\n\nEDITED COPY\n")); // an unchanged source must not be converted again
    e.run(&["--no-popup", "rust"]);
    assert!(read(&copy).contains("EDITED COPY"));
    std::thread::sleep(Duration::from_millis(20));
    write(&e.lib.join("rust/memory-a.md"), "# A changed");
    e.run(&["--no-popup", "rust"]);
    assert!(read(&copy).contains("A changed"));
    assert!(!read(&copy).contains("EDITED"));
}

#[test]
fn copy_remade_when_converter_version_changes() {
    let e = Env::new();
    e.run(&["--no-popup", "rust"]);
    let copy = e.out.join("rust - memory-a.txt");
    write(&copy, "SOURCE: x\n\nOLD FORMAT\n");
    let idx = e.out.join(".index.json");
    let mut index: Value = serde_json::from_str(&read(&idx)).unwrap();
    index["rust/memory-a"]["v"] = Value::from(1); // a copy made by an older converter
    write(&idx, &index.to_string());
    e.run(&["--no-popup", "rust"]);
    assert!(!read(&copy).contains("OLD FORMAT"));
}

#[test]
fn text_dir_in_config() {
    let e = Env::new();
    let elsewhere = e.tmp.join("mytext");
    write(&e.cfg, &format!("text_dir = \"{}\"\n{}", elsewhere.display(), read(&e.cfg)));
    let o = e.cmd().arg("--config").arg(&e.cfg).args(["--no-popup", "rust"]).output().unwrap();
    assert!(o.status.success());
    assert!(elsewhere.join("rust - linux.txt").is_file());
}

#[test]
fn saved_default_respected() {
    let e = Env::new();
    write(
        &e.conf_dir.join("choices.json"),
        r#"{"rust": {"selected": ["rust/linux"], "known": ["rust/linux", "rust/memory-a", "rust/memory-b"]}}"#,
    );
    let (_, out) = e.run(&["--no-popup", "rust"]);
    assert!(out.contains("total: 1 docs"), "{out}");
    assert!(out.contains("left out by choice (2)"), "{out}");
}

#[test]
fn long_left_out_list_is_only_counted() {
    let e = Env::new();
    for i in 0..25 {
        write(&e.lib.join(format!("many/d{i:02}.md")), &format!("doc {i}"));
    }
    write(&e.conf_dir.join("choices.json"), r#"{"mine": {"selected": ["web/seo"], "known": [], "custom": true}}"#);
    let (code, out) = e.run(&["--no-popup", "mine"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("left out by choice (29): not named (more than 20)"), "{out}");
    assert!(!out.contains("many/d00"), "{out}");
}

#[test]
fn set_names_keep_their_case() {
    let e = Env::new();
    let cfg = read(&e.cfg);
    write(&e.cfg, &format!("{cfg}[sets.MyRust]\ndocs = [\"rust/linux\"]\n"));
    let (code, out) = e.run(&["--no-popup", "MyRust"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("total: 1 docs"), "{out}");
}

#[test]
fn extra_doc_from_library_is_loaded_with_saved_preset() {
    let e = Env::new();
    write(
        &e.conf_dir.join("choices.json"),
        r#"{"rust": {"selected": ["rust/linux", "web/seo"], "known": ["rust/linux", "rust/memory-a", "rust/memory-b"]}}"#,
    );
    let (_, out) = e.run(&["--no-popup", "rust"]);
    assert!(out.contains("total: 2 docs"), "{out}");
    assert!(out.contains("web - seo.txt"), "{out}");
}

#[test]
fn setup_saves_defaults_and_loads_nothing() {
    let e = Env::new();
    let (code, out) = e.run(&["--no-popup", "--setup", "rust"]);
    assert_eq!(code, 0);
    assert!(out.contains("SAVED default for rust: 3 of 3 docs"), "{out}");
    assert!(out.contains("setup only: nothing loaded"));
    let saved: Value = serde_json::from_str(&read(&e.conf_dir.join("choices.json"))).unwrap();
    assert_eq!(saved["rust"]["selected"].as_array().unwrap().len(), 3);
    assert!(!e.usage_file().exists());
}

#[test]
fn text_folder_inside_the_library_is_refused() {
    let e = Env::new();
    let o = e.cmd().arg("--config").arg(&e.cfg).arg("--out").arg(&e.lib).arg("--sync").output().unwrap();
    assert_eq!(o.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&o.stdout).contains("is inside the library"));
    assert_eq!(read(&e.lib.join("rust/memory-b.txt")), "B"); // the user's own file is untouched
}

#[test]
fn unreadable_choices_are_kept_aside_not_wiped() {
    let e = Env::new();
    write(&e.conf_dir.join("choices.json"), "{bad");
    assert_eq!(e.run(&["--setup", "rust"]).0, 0);
    assert_eq!(read(&e.conf_dir.join("choices.json.bad")), "{bad");
    assert!(read(&e.conf_dir.join("choices.json")).contains("\"rust\""));
}

#[cfg(unix)]
#[test]
fn a_new_text_folder_inside_a_symlinked_library_is_refused() {
    let e = Env::new();
    let link = e.tmp.join("liblink");
    std::os::unix::fs::symlink(&e.lib, &link).unwrap();
    let out = link.join("copies");
    let o = e.cmd().arg("--config").arg(&e.cfg).arg("--out").arg(&out).arg("--sync").output().unwrap();
    assert_eq!(o.status.code(), Some(6));
    assert!(!e.lib.join("copies").exists());
}

#[cfg(unix)]
#[test]
fn choices_that_cannot_be_read_are_kept_aside() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    let p = e.conf_dir.join("choices.json");
    write(&p, r#"{"mine": {"selected": []}}"#);
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&p).is_ok() {
        return; // root reads anything
    }
    assert_eq!(e.run(&["--no-popup", "--setup", "rust"]).0, 0);
    let bad = e.conf_dir.join("choices.json.bad");
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read(&bad).contains("mine"));
}

#[cfg(unix)]
#[test]
fn a_symlinked_config_stays_a_link() {
    let e = Env::new();
    let real = e.tmp.join("dotfiles/config.toml");
    write(&real, &read(&e.cfg));
    fs::remove_file(&e.cfg).unwrap();
    std::os::unix::fs::symlink(&real, &e.cfg).unwrap();
    assert_eq!(e.run(&["--usage-share", "off"]).0, 0);
    assert!(fs::symlink_metadata(&e.cfg).unwrap().file_type().is_symlink());
    assert!(read(&real).contains("share = false"));
}

#[test]
fn list() {
    let e = Env::new();
    let (code, out) = e.run(&["--list"]);
    assert_eq!(code, 0);
    assert!(out.contains("library:"));
    assert!(out.contains("rust"));
    assert!(out.contains("all"));
    assert!(out.contains("aliases: rs=rust"));
}

#[test]
fn init() {
    let e = Env::new();
    fs::remove_file(&e.cfg).unwrap();
    let lib = e.lib.display().to_string();
    let o = e.cmd().arg("--config").arg(&e.cfg).args(["--init", &lib]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let cfg: toml::Table = read(&e.cfg).parse().unwrap();
    assert_eq!(cfg["library"].as_str(), Some(lib.as_str()));
    let o = e.cmd().arg("--config").arg(&e.cfg).args(["--init", &lib]).output().unwrap();
    assert_eq!(o.status.code(), Some(1)); // no overwrite
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn example_library_loads() {
    let e = Env::new();
    let lib = repo().join("examples/library");
    let (code, out) = e.run(&["--library", lib.to_str().unwrap(), "--no-popup", "all"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("total: 8 docs"), "{out}");
}

#[test]
fn example_config_parses() {
    let cfg: toml::Table = read(&repo().join("examples/config.toml")).parse().unwrap();
    assert!(cfg["sets"].as_table().unwrap().contains_key("myapp"));
}

// ---------------------------------------------------------------- sessions

fn jl(path: &Path, entries: &[Value], age_secs: u64) {
    let body: Vec<String> = entries.iter().map(Value::to_string).collect();
    write(path, &(body.join("\n") + "\nnot json\n"));
    let t = SystemTime::now() - Duration::from_secs(age_secs);
    fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

fn user(text: &str) -> Value {
    serde_json::json!({"type": "user", "timestamp": "2026-11-16T10:00:00Z", "cwd": "/work/app",
                       "message": {"role": "user", "content": text}})
}

fn assistant(text: &str) -> Value {
    serde_json::json!({"type": "assistant", "timestamp": "2026-11-16T10:01:00Z",
        "message": {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "hmm"}, {"type": "text", "text": text},
            {"type": "tool_use", "name": "Bash", "input": {"command": "secret-tool-call"}}]}})
}

struct Sess {
    e: Env,
    proj: PathBuf,
    reg: PathBuf,
}

fn sessions_env() -> Sess {
    let e = Env::new();
    let proj = e.tmp.join("projects");
    e.append_cfg(&format!("\n[sessions]\nenabled = true\nprojects_dir = \"{}\"\n", proj.display()));
    let d = proj.join("-work-app");
    let mut meta = user("<system-reminder>ignore me</system-reminder>");
    meta["isMeta"] = Value::Bool(true);
    jl(
        &d.join(format!("{SID1}.jsonl")),
        &[
            serde_json::json!({"type": "file-history-snapshot"}),
            meta,
            user("<command-message>x</command-message>"),
            user("Fix the popup crash on   start-up"),
            assistant("Found it, the timeout was wrong."),
            serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "TOOLOUT"}]}}),
        ],
        3600,
    );
    jl(
        &d.join(format!("{SID2}.jsonl")),
        &[user("Second one"), serde_json::json!({"type": "ai-title", "aiTitle": "Nice title"})],
        3600,
    );
    jl(&d.join(format!("{SID3}.jsonl")), &[user("Still open")], 5); // modified just now
    jl(&d.join("agent-abc.jsonl"), &[user("not a session id")], 3600);
    let reg = e.conf_dir.join("sessions.json");
    Sess { e, proj, reg }
}

#[test]
fn sessions_off_by_default() {
    let s = sessions_env();
    write(&s.e.cfg, &format!("library = \"{}\"\n", s.e.lib.display()));
    let (code, out) = s.e.run(&["--sessions"]);
    assert_eq!(code, 7);
    assert!(out.contains("SESSIONS OFF"));
    assert!(!s.reg.exists());
}

#[test]
fn lists_closed_sessions_with_full_id() {
    let s = sessions_env();
    let (code, out) = s.e.run(&["--sessions"]);
    assert_eq!(code, 0);
    assert!(out.contains(&format!("{SID1}  2026-11-16 10:01  Fix the popup crash on start-up")), "{out}");
    assert!(out.contains(SID2));
    assert!(out.contains("Nice title")); // Claude Code's own title wins over the first prompt
    assert!(!out.contains(SID3)); // still open: not tracked yet
    assert!(!out.contains("agent-abc"));
    assert!(out.contains("2 closed sessions"));
}

#[test]
fn moved_chats_are_found_in_extra_dirs() {
    let s = sessions_env();
    s.e.run(&["--sessions"]);
    s.e.run(&["--rename", SID1, "popup crash"]);
    // /clear moved SID1 to <cache>/<date>_<sid>/projects/<proj>/, SID4 sits in a plain project folder
    let cache = s.e.tmp.join("cache dir");
    let moved = cache.join(format!("2026-11-24_{SID1}")).join("projects").join("-work-app");
    fs::create_dir_all(&moved).unwrap();
    fs::rename(s.proj.join("-work-app").join(format!("{SID1}.jsonl")), moved.join(format!("{SID1}.jsonl"))).unwrap();
    jl(&cache.join("-work-old").join(format!("{SID4}.jsonl")), &[user("Old archived chat")], 3600);
    // a stale copy of SID2 in the cache: the live one in projects_dir wins
    jl(&cache.join("-work-app").join(format!("{SID2}.jsonl")), &[user("stale copy")], 3600);
    s.e.append_cfg(&format!("extra_dirs = [\"{}\"]\n", cache.display()));
    let (code, out) = s.e.run(&["--sessions"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("popup crash"), "{out}"); // the name followed the chat
    assert!(out.contains("Old archived chat"), "{out}");
    assert!(out.contains("Nice title") && !out.contains("stale copy"), "{out}");
    let reg: Value = serde_json::from_str(&fs::read_to_string(&s.reg).unwrap()).unwrap();
    assert!(reg["sessions"][SID1]["file"].as_str().unwrap().contains("cache dir"));
}

#[test]
fn rename_sticks_and_keeps_the_id() {
    let s = sessions_env();
    s.e.run(&["--sessions"]);
    let (code, out) = s.e.run(&["--rename", &SID1[..8], "popup crash"]);
    assert_eq!(code, 0);
    assert!(out.contains(&format!("\"popup crash\" ({SID1})")), "{out}");
    let (_, out) = s.e.run(&["--sessions"]); // a rescan must not undo a manual name
    assert!(out.contains(&format!("{SID1}  2026-11-16 10:01  popup crash")), "{out}");
    let reg: Value = serde_json::from_str(&read(&s.reg)).unwrap();
    assert_eq!(reg["sessions"][SID1]["name_source"], "manual");
}

#[test]
fn auto_name_follows_a_later_title() {
    let s = sessions_env();
    s.e.run(&["--sessions"]);
    let path = s.proj.join("-work-app").join(format!("{SID1}.jsonl"));
    jl(
        &path,
        &[user("Fix the popup crash"), serde_json::json!({"type": "custom-title", "customTitle": "Crash hunt"})],
        3000,
    );
    let (_, out) = s.e.run(&["--sessions"]);
    assert!(out.contains("Crash hunt"), "{out}");
}

#[test]
fn ambiguous_and_unknown_ids() {
    let s = sessions_env();
    s.e.run(&["--sessions"]);
    let (code, out) = s.e.run(&["--resume", "ffff"]);
    assert_eq!(code, 8);
    assert!(out.contains("no session matches"));
    assert_eq!(s.e.run(&["--rename", "", "x"]).0, 8);
}

#[test]
fn resume_prints_cd_and_full_id() {
    let s = sessions_env();
    s.e.run(&["--sessions"]);
    let (code, out) = s.e.run(&["--resume", SID1]);
    assert_eq!(code, 0);
    assert!(out.contains(&format!("cd /work/app && claude --resume {SID1}")), "{out}");
}

#[test]
fn session_load_writes_conversation_without_tool_traffic() {
    let s = sessions_env();
    s.e.run(&["--sessions"]);
    let (code, out) = s.e.run(&["--session-load", &SID1[..6]]);
    assert_eq!(code, 0, "{out}");
    let path =
        out.lines().find_map(|l| l.split_once("chars  ").filter(|_| !l.contains("total")).map(|(_, p)| p)).unwrap();
    let text = read(Path::new(path));
    assert!(text.contains(&format!("SESSION ID: {SID1}")));
    assert!(text.contains("USER: Fix the popup crash on   start-up"));
    assert!(text.contains("ASSISTANT: Found it"));
    for bad in ["ignore me", "TOOLOUT", "secret-tool-call", "hmm", "command-message"] {
        assert!(!text.contains(bad), "{bad}");
    }
}

#[test]
fn sessions_need_no_library() {
    let s = sessions_env();
    write(&s.e.cfg, &format!("[sessions]\nenabled = true\nprojects_dir = \"{}\"\n", s.proj.display()));
    assert_eq!(s.e.run(&["--sessions"]).0, 0);
}

// ---------------------------------------------------------------- --context: earlier chats

const SID4: &str = "44444444-4444-4444-8444-444444444444";

/// The sessions setup plus a newer chat in a second folder.
fn context_env() -> (Sess, PathBuf) {
    let s = sessions_env();
    let entry = serde_json::json!({"type": "user", "timestamp": "2026-11-17T09:00:00Z", "cwd": "/work/web",
                                   "message": {"role": "user", "content": "Newest chat, other folder"}});
    jl(&s.proj.join("-work-web").join(format!("{SID4}.jsonl")), &[entry], 3600);
    let presets = s.e.conf_dir.join("contexts.json");
    (s, presets)
}

fn save_preset(path: &Path, name: &str, selected: &[&str]) {
    write(path, &serde_json::json!({name: {"selected": selected, "known": []}}).to_string());
}

fn known(path: &Path, name: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(&read(path)).unwrap();
    v[name]["known"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
}

#[test]
fn context_needs_sessions_on() {
    let (s, _) = context_env();
    write(&s.e.cfg, &format!("library = \"{}\"\n", s.e.lib.display()));
    let (code, out) = s.e.run(&["--context"]);
    assert_eq!(code, 7);
    assert!(out.contains("SESSIONS OFF"));
}

#[test]
fn context_new_preset_without_popup_loads_nothing() {
    let (s, _) = context_env();
    let (code, out) = s.e.run(&["--context", "noname"]);
    assert_eq!(code, 5);
    assert!(out.contains("context preset noname has no saved choice yet"), "{out}");
    assert!(!s.e.out.exists());
}

#[test]
fn context_setup_saves_all_chats_newest_folder_first() {
    let (s, presets) = context_env();
    assert_eq!(s.e.run(&["--context", "--setup", "work"]).0, 5);
    save_preset(&presets, "work", &[]);
    let (code, out) = s.e.run(&["--context", "--setup", "work"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("SAVED context preset work: 0 of 4 chats"), "{out}");
    // /work/web has the newest chat; then newest first (ties by id); the open SID3 is offered
    assert_eq!(known(&presets, "work"), [SID4, SID1, SID2, SID3]);
}

#[test]
fn context_never_offers_the_running_chat() {
    let (s, presets) = context_env();
    save_preset(&presets, "work", &[]);
    s.e.run_with(&["--context", "--setup", "work"], &[("CLAUDE_CODE_SESSION_ID", SID3)]);
    assert!(!known(&presets, "work").contains(&SID3.to_string()));
}

#[test]
fn the_running_chat_in_its_own_preset_is_not_missing() {
    let (s, presets) = context_env();
    save_preset(&presets, "work", &[]);
    s.e.run(&["--context", "--setup", "work"]); // registers SID3 while it is not running
    save_preset(&presets, "work", &[SID2, SID3]);
    let (code, out) = s.e.run_with(&["--context", "work"], &[("CLAUDE_CODE_SESSION_ID", SID3)]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("MISSING"), "{out}");
}

#[test]
fn context_preset_loads_chats_named_by_first_prompt() {
    let (s, presets) = context_env();
    save_preset(&presets, "work", &[SID2, SID4, "99999999-9999-4999-8999-999999999999"]);
    let (code, out) = s.e.run(&["--context", "work"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("CHAT \"Second one\" ({SID2})")), "{out}"); // first prompt, not the AI title
    assert!(out.contains(&format!("CHAT \"Newest chat, other folder\" ({SID4})")));
    assert!(out.contains("MISSING  chat 99999999-9999-4999-8999-999999999999"));
    assert!(out.contains("total: 2 chats"));
    assert!(!out.contains(SID1));
    let paths: Vec<&str> =
        out.lines().filter(|l| !l.contains("total")).filter_map(|l| l.split_once("chars  ").map(|(_, p)| p)).collect();
    assert_eq!(paths.len(), 2);
    assert!(read(Path::new(paths[0])).contains("USER: Newest chat")); // in list order
    assert!(read(Path::new(paths[1])).contains("USER: Second one"));
}

#[test]
fn context_manual_name_wins_over_first_prompt() {
    let (s, presets) = context_env();
    s.e.run(&["--sessions"]);
    s.e.run(&["--rename", SID2, "my name"]);
    save_preset(&presets, "work", &[SID2]);
    let (_, out) = s.e.run(&["--context", "work"]);
    assert!(out.contains(&format!("CHAT \"my name\" ({SID2})")), "{out}");
}

#[test]
fn context_leaves_open_chats_out_of_the_sessions_list() {
    let (s, presets) = context_env();
    save_preset(&presets, "work", &[]);
    s.e.run(&["--context", "--setup", "work"]);
    let (_, out) = s.e.run(&["--sessions"]);
    assert!(!out.contains(SID3));
    assert!(out.contains("3 closed sessions"), "{out}");
}

#[test]
fn context_list_and_bad_names() {
    let (s, presets) = context_env();
    assert!(s.e.run(&["--context", "--list"]).1.contains("no context presets saved yet"));
    save_preset(&presets, "work", &[SID1]);
    let (_, out) = s.e.run(&["--context", "--list"]);
    assert!(out.contains("work") && out.contains("1 chats"), "{out}");
    assert_eq!(s.e.run(&["--context", "a/b"]).0, 2);
}

#[test]
fn an_empty_rename_gives_the_automatic_name_back() {
    let s = sessions_env();
    s.e.run(&["--sessions"]);
    s.e.run(&["--rename", SID1, "popup crash"]);
    let (code, out) = s.e.run(&["--rename", SID1, "  "]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("\"Fix the popup crash on start-up\" ({SID1})")), "{out}");
    let reg: Value = serde_json::from_str(&read(&s.reg)).unwrap();
    assert_eq!(reg["sessions"][SID1]["name_source"], "auto");
    // and a later title is followed again, as for any automatic name
    let (_, out) = s.e.run(&["--sessions"]);
    assert!(out.contains(&format!("{SID1}  2026-11-16 10:01  Fix the popup crash on start-up")), "{out}");
}

// ---------------------------------------------------------------- one window, two tabs

/// Saved research defaults for `rust` and a saved chat preset `default`.
fn both_env() -> (Sess, PathBuf) {
    let (s, presets) = context_env();
    write(
        &s.e.conf_dir.join("choices.json"),
        r#"{"rust": {"selected": ["rust/memory-a"], "known": ["rust/linux", "rust/memory-a", "rust/memory-b"]}}"#,
    );
    save_preset(&presets, "default", &[SID2]);
    (s, presets)
}

#[test]
fn without_a_window_only_the_own_side_loads_and_no_chat_is_scanned() {
    let (s, _) = both_env();
    // the Chats tab only loads when it is clicked: no window, no click, no scan
    let (code, out) = s.e.run(&["rust"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("total: 1 docs") && !out.contains("CHAT ") && !out.contains("chats"), "{out}");
    assert!(!s.reg.exists(), "a research load scanned the chats");
    assert_eq!(s.e.records().len(), 1);
    // and the other way round: a chat load does not touch the research
    let (code, out) = s.e.run(&["--context", "default", "--start", "research"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("CHAT \"Second one\" ({SID2})")) && !out.contains("docs"), "{out}");
    assert_eq!(s.e.records().len(), 2);
    assert_eq!(s.e.run(&["rust", "--start", "nope"]).0, 2); // clap refuses it
    // a problem on the other side is not this command's problem
    write(&s.e.cfg, &(read(&s.e.cfg).replace("default_set = \"rust\"", "default_set = \"ghost\"")));
    assert_eq!(s.e.run(&["--context", "default"]).0, 0);
}

#[test]
fn a_display_name_shows_before_the_path() {
    let (s, _) = both_env();
    write(&s.e.conf_dir.join("names.json"), r#"{"docs": {"rust/memory-a": "Memory, part A"}}"#);
    let (code, out) = s.e.run(&["rust"]);
    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let i = lines.iter().position(|l| *l == "DOC \"Memory, part A\" (rust/memory-a)").expect("DOC line");
    assert!(lines[i + 1].contains("chars  ") && lines[i + 1].ends_with(".txt"), "{out}");
    assert!(!s.e.lib.join("rust/memory-a.md").to_string_lossy().is_empty());
    assert_eq!(read(&s.e.lib.join("rust/memory-a.md")), "# A"); // the library is never touched
}

// ---------------------------------------------------------------- project profiles

/// A profile `app` over a small source tree, with a saved preset `gui`.
fn profile_env() -> (Sess, PathBuf) {
    let (s, presets) = context_env();
    let root = s.e.tmp.join("repo");
    write(&root.join("src/main.rs"), "fn main() {}");
    write(&root.join("src/gui/popup.rs"), "// popup");
    write(&root.join("docs/notes.md"), "notes");
    write(&root.join("target/x.rs"), "built");
    s.e.append_cfg(&format!("\n[profiles.app]\nroot = \"{}\"\ncode = [\"src/*\"]\n", root.display()));
    let gui = serde_json::json!({"app": {"presets": {"gui": {
        "code": ["src/gui/popup.rs"], "chats": [SID2], "research": ["rust/memory-a"]}}}});
    write(&s.e.conf_dir.join("profiles.json"), &gui.to_string());
    (s, presets)
}

#[test]
fn a_profile_root_that_is_not_text_is_a_config_error() {
    let e = Env::new();
    e.append_cfg("\n[profiles.p]\nroot = 5\n");
    let (code, out) = e.run(&["--profile", "p"]);
    assert_eq!(code, 6, "{out}");
    assert!(out.contains("CONFIG ERROR: [profiles.p] root must be a string"), "{out}");
}

#[cfg(unix)]
#[test]
fn a_named_pipe_in_the_code_root_is_skipped() {
    let (s, _) = profile_env();
    let pipe = s.e.tmp.join("repo/src/events.pipe");
    assert!(Command::new("mkfifo").arg(&pipe).status().unwrap().success());
    let (code, out) = s.e.run(&["--profile", "app", "gui"]); // hung on the read before
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("events.pipe"), "{out}");
}

#[test]
fn a_profile_preset_loads_code_chats_and_research_without_a_window() {
    let (s, _) = profile_env();
    let (code, out) = s.e.run(&["--profile", "app", "gui"]);
    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let c = lines.iter().position(|l| *l == "CODE src/gui/popup.rs").expect("CODE line");
    assert!(lines[c + 1].ends_with("src/gui/popup.rs") && lines[c + 1].contains("bytes  "), "{out}");
    assert!(!out.contains("src/main.rs"), "{out}"); // not in the preset
    let chat = lines.iter().position(|l| l.starts_with("CHAT \"Second one\"")).expect("CHAT line");
    let doc = lines.iter().position(|l| l.contains("chars  ") && l.contains("memory-a")).expect("research line");
    assert!(c < chat && chat < doc, "code, chats, research: {out}");
    // research and chats are counted as always; code has no usage record
    let recs = s.e.records();
    assert_eq!(recs.len(), 2, "{out}");
    assert_eq!(recs.iter().map(|r| r["fmt"]["chat"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 0]); // chats print first
    // a profile load saves no set default or chat preset
    assert!(!s.e.conf_dir.join("choices.json").exists());
    assert!(!s.e.conf_dir.join("contexts.json").exists());
}

#[test]
fn a_code_only_preset_writes_no_usage_record() {
    let (s, _) = profile_env();
    write(
        &s.e.conf_dir.join("profiles.json"),
        r#"{"app": {"presets": {"src": {"code": ["src/main.rs"], "chats": [], "research": []}}}}"#,
    );
    let (code, out) = s.e.run(&["--profile", "app", "src", "--start", "code"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("CODE src/main.rs") && out.contains("total: 1 files"), "{out}");
    assert!(!s.e.usage_file().exists(), "{out}");
    assert!(!s.reg.exists(), "a code-only preset scanned the chats");
}

#[test]
fn profile_list_unknown_preset_and_no_window() {
    let (s, _) = profile_env();
    let (code, out) = s.e.run(&["--profile", "app", "--list"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("gui") && out.contains("1 files") && out.contains("1 chats") && out.contains("1 docs"),
        "{out}"
    );
    let (code, out) = s.e.run(&["--profile", "app", "nope"]);
    assert_eq!(code, 2);
    assert!(out.contains("unknown preset nope for profile app; known: gui"), "{out}");
    let (code, out) = s.e.run(&["--profile", "app"]);
    assert_eq!(code, 5);
    assert!(out.contains("name a preset"), "{out}");
    assert_eq!(s.e.run(&["--profile", "Bad/Name"]).0, 2);
    s.e.append_cfg("[profiles.gone]\nroot = \"/no/such/folder\"\n");
    assert_eq!(s.e.run(&["--profile", "gone", "--list"]).0, 6);
}

// ---------------------------------------------------------------- parity with the Python version

/// Both versions share the text copy (same TEXT_VERSION), so they must write the same text.
#[test]
fn same_text_copies_as_python() {
    let py = repo().join("skills/researcher/researcher.py");
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 not found: parity check skipped");
        return;
    }
    let e = Env::new();
    let lib = repo().join("examples/library");
    write(
        &e.lib.join("extra/entities.html"),
        "<p>caf&eacute; &amp; &#8211; &nbsp;x</p><table><tr><td>a</td><td>b</td></tr></table><!-- hidden > tail --><p>y</p><!-->z<!-- never closed <p>w</p>",
    );
    write(
        &e.lib.join("extra/rules.md"),
        "Title\r\n=====\r\n\r\n\r\n- a\n    - b\n\u{200b}----------\n<!-- hidden > tail -->after\n<!--\nmulti\nline\n-->end\n",
    );
    for (name, l) in [("rs", lib.as_path()), ("rs2", e.lib.as_path())] {
        let rs_out = e.tmp.join(name);
        let py_out = e.tmp.join(format!("{name}-py"));
        let o = e
            .cmd()
            .args(["--library", l.to_str().unwrap(), "--out", rs_out.to_str().unwrap(), "--sync"])
            .output()
            .unwrap();
        assert!(o.status.success());
        let o = Command::new("python3")
            .arg("-I")
            .arg(&py)
            .args(["--library", l.to_str().unwrap(), "--out", py_out.to_str().unwrap(), "--sync"])
            .env("RESEARCHER_NO_POPUP", "1")
            .env("XDG_DATA_HOME", &e.data)
            .env("XDG_CONFIG_HOME", e.tmp.join("xdg-config"))
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let mut files: Vec<String> = fs::read_dir(&py_out)
            .unwrap()
            .flatten()
            .map(|x| x.file_name().to_string_lossy().into_owned())
            .filter(|f| f.ends_with(".txt"))
            .collect();
        files.sort();
        assert!(!files.is_empty());
        for f in files {
            assert_eq!(read(&rs_out.join(&f)), read(&py_out.join(&f)), "{f} differs");
        }
        let rs_idx: Value = serde_json::from_str(&read(&rs_out.join(".index.json"))).unwrap();
        let py_idx: Value = serde_json::from_str(&read(&py_out.join(".index.json"))).unwrap();
        assert_eq!(rs_idx, py_idx, "the indexes differ");
    }
}

/// `--usage-share` edits the user's config by hand-written text rules: both versions must
/// write the same file, and the same dry-run request.
#[test]
fn same_config_edits_and_requests_as_python() {
    let py = repo().join("skills/researcher/researcher.py");
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 not found: parity check skipped");
        return;
    }
    let e = Env::new();
    let cases = [
        "",
        "library = \"/r\"",
        "library = \"/r\"\n",
        "library = \"/r\"\n[usage]",
        "[usage] # mine\r\nlog = true\r\n[sets.a]\r\ndocs = [\"x\"]\r\n",
        "# [usage]\n# share = true\n[aliases]\na = \"b\"\n",
        "[usage]\nshare = false # old\nurl = \"http://x\"\n\n[sets.share]\ndocs = []\n",
    ];
    for (i, case) in cases.iter().enumerate() {
        for answer in ["on", "off"] {
            let (rs, pyc) = (e.tmp.join(format!("rs{i}{answer}.toml")), e.tmp.join(format!("py{i}{answer}.toml")));
            write(&rs, case);
            write(&pyc, case);
            let a = e.cmd().arg("--config").arg(&rs).args(["--usage-share", answer]).output().unwrap();
            let b = Command::new("python3")
                .arg("-I")
                .arg(&py)
                .arg("--config")
                .arg(&pyc)
                .args(["--usage-share", answer])
                .env("XDG_DATA_HOME", &e.data)
                .output()
                .unwrap();
            assert_eq!(a.status.code(), Some(0), "case {i}: {}", String::from_utf8_lossy(&a.stdout));
            assert_eq!(b.status.code(), Some(0), "case {i}: {}", String::from_utf8_lossy(&b.stdout));
            assert_eq!(read(&rs), read(&pyc), "case {i} {answer}: the configs differ");
        }
    }

    write(&e.usage_file(), &format!("{REC}\nnot json\n{REC}\n"));
    let a = e.run(&["--usage-send", "--dry-run"]).1;
    let b = Command::new("python3")
        .arg("-I")
        .arg(&py)
        .arg("--config")
        .arg(&e.cfg)
        .args(["--usage-send", "--dry-run"])
        .env("XDG_DATA_HOME", &e.data)
        .output()
        .unwrap();
    assert_eq!(a, String::from_utf8_lossy(&b.stdout), "the dry runs differ");
}

/// The same load gives the same record in both versions (only the random install id differs),
/// the same `--usage` report, and a chat load too.
#[test]
fn same_usage_records_as_python() {
    let py = repo().join("skills/researcher/researcher.py");
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 not found: parity check skipped");
        return;
    }
    let (s, presets) = context_env();
    save_preset(&presets, "work", &[SID2, SID4]);
    let py_data = s.e.tmp.join("py-data");
    let py_run = |args: &[&str]| {
        let o = Command::new("python3")
            .arg("-I")
            .arg(&py)
            .arg("--config")
            .arg(&s.e.cfg)
            .arg("--out")
            .arg(s.e.tmp.join("py-out"))
            .args(args)
            .env("RESEARCHER_NO_POPUP", "1")
            .env("XDG_DATA_HOME", &py_data)
            .env("XDG_CACHE_HOME", s.e.tmp.join("py-cache"))
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    for args in [&["--no-popup", "rust"][..], &["--no-popup", "app"], &["--context", "work"], &["--sessions"]] {
        s.e.run(args);
        py_run(args);
    }
    s.e.run(&["--session-load", &SID1[..6]]);
    py_run(&["--session-load", &SID1[..6]]);
    let rs: Vec<Value> = s.e.records();
    let pyr: Vec<Value> =
        read(&py_data.join("researcher/usage.jsonl")).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rs.len(), 4, "rust: {rs:?}"); // 2 library loads, a context load, a session load
    let strip = |v: &Value| {
        let mut v = v.clone();
        v["id"] = Value::Null;
        v
    };
    assert_eq!(rs.iter().map(strip).collect::<Vec<_>>(), pyr.iter().map(strip).collect::<Vec<_>>());
    let report = |o: String| o.lines().filter(|l| !l.starts_with("usage log:")).collect::<Vec<_>>().join("\n");
    assert_eq!(report(s.e.run(&["--usage"]).1), report(py_run(&["--usage"])));
}

// ---------------------------------------------------------------- first use

#[test]
fn no_library_without_window_asks_nothing() {
    // with RESEARCHER_NO_POPUP the first-use window must not open; the old message stays
    let e = Env::new();
    write(&e.cfg, "default_set = \"x\"\n");
    let (code, out) = e.run(&["x"]);
    assert_eq!(code, 6);
    assert!(out.contains("NO LIBRARY"), "{out}");
    assert_eq!(read(&e.cfg), "default_set = \"x\"\n"); // config untouched
}

#[test]
fn no_display_falls_back_to_the_message() {
    let e = Env::new();
    write(&e.cfg, "");
    let o = e
        .cmd()
        .env_remove("RESEARCHER_NO_POPUP")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .arg("--config")
        .arg(&e.cfg)
        .arg("rust")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(6), "{out}");
    assert!(out.contains("NO LIBRARY") && out.contains("no window to ask"), "{out}");
}

#[test]
fn empty_library_says_so() {
    let e = Env::new();
    let empty = e.tmp.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let (code, out) = e.run(&["--library", empty.to_str().unwrap(), "--no-popup", "rust"]);
    assert_eq!(code, 6);
    assert!(out.contains("EMPTY LIBRARY"), "{out}");
    let (code, out) = e.run(&["--library", empty.to_str().unwrap(), "--list"]);
    assert_eq!(code, 0, "{out}");
}

// ---------------------------------------------------------------- chats count as uses too

#[test]
fn a_context_load_is_logged_and_counted() {
    let (s, presets) = context_env();
    save_preset(&presets, "work", &[SID2, SID4]);
    let (code, out) = s.e.run(&["--context", "work"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.trim_end().ends_with("USAGE SHARING: not decided (researcher --usage-share on|off)"), "{out}");
    let recs = s.e.records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert_eq!(r["v"], 3);
    assert_eq!(r["docs"], 2);
    assert_eq!(r["vs_chars"], r["fmt_chars"]); // a chat is never counted against a file format
    assert_eq!(r["fmt"], serde_json::json!({"pdf": 0, "html": 0, "md": 0, "txt": 0, "chat": 2}));
    assert_eq!(r["fmt_chars"]["chat"], r["chars"]);
    assert_eq!(r["raw"], r["chars"]); // a chat is read as written: nothing squeezed
    assert!(r["src_bytes"].as_u64() > Some(0));
    // every chat that could have been loaded is the library of this load: 4 chats here
    assert_eq!(r["lib_docs"], 4);
    assert!(r["lib_chars"].as_u64() > r["chars"].as_u64());
    // no chat text, name or folder in the log
    let raw = read(&s.e.usage_file());
    for word in ["Second one", "Newest chat", "work", "web", "popup"] {
        assert!(!raw.contains(word), "{word} in the log");
    }
    assert!(e_usage_report(&s.e).contains("1 loads"));
}

#[test]
fn a_chat_load_is_compared_with_the_chats_up_to_the_newest_picked() {
    let (s, presets) = context_env();
    // SID1 is older than the chat in the other folder: that newer chat is not part of the saving
    save_preset(&presets, "work", &[SID1]);
    let (code, out) = s.e.run(&["--context", "work"]);
    assert_eq!(code, 0, "{out}");
    let r = &s.e.records()[0];
    assert_eq!(r["lib_docs"], 3, "{out}");
    // picking the newest chat brings every chat into the comparison again
    save_preset(&presets, "work2", &[SID1, SID4]);
    let (code, out) = s.e.run(&["--context", "work2"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(s.e.records()[1]["lib_docs"], 4, "{out}");
}

fn e_usage_report(e: &Env) -> String {
    e.run(&["--usage"]).1
}

#[test]
fn a_session_load_is_logged_and_counted() {
    let s = sessions_env();
    s.e.run(&["--sessions"]);
    let (code, out) = s.e.run(&["--session-load", &SID1[..6]]);
    assert_eq!(code, 0, "{out}");
    let recs = s.e.records();
    assert_eq!(recs.len(), 1, "{out}");
    assert_eq!(recs[0]["fmt"]["chat"], 1);
    assert_eq!(recs[0]["docs"], 1);
    assert!(recs[0]["lib_docs"].as_u64() >= Some(2));
}

#[test]
fn listing_and_cancelled_chats_load_nothing_so_log_nothing() {
    let (s, presets) = context_env();
    save_preset(&presets, "work", &[SID2]);
    s.e.run(&["--context", "--list"]);
    s.e.run(&["--sessions"]);
    s.e.run(&["--context", "--setup", "work"]);
    assert!(!s.e.usage_file().exists());
}

#[test]
fn chat_loads_are_sent_like_library_loads() {
    let (s, presets) = context_env();
    let (url, got) = receiver(vec![(200, r#"{"ok":true,"received":1,"new":1}"#)]);
    s.e.share_on(&url);
    save_preset(&presets, "work", &[SID2]);
    let (code, out) = s.e.run(&["--context", "work"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("USAGE SHARING"), "{out}");
    let body = got.recv_timeout(Duration::from_secs(20)).expect("the background send arrives");
    assert_eq!(body["schema"], 2);
    assert_eq!(body["loads"][0]["v"], 3);
    assert_eq!(body["loads"][0]["fmt"]["chat"], 1);
}

#[test]
fn old_version_1_lines_are_still_read_and_sent() {
    let e = Env::new();
    e.log_lines(1); // REC is a version 1 line
    let (_, out) = e.run(&["--usage"]);
    assert!(out.contains("1 loads"), "{out}");
    assert!(out.contains("0 of 1 loads say their formats"), "{out}");
    let (_, out) = e.run(&["--usage-send", "--dry-run"]);
    assert!(out.starts_with("DRY RUN: 1 records"), "{out}");
}

#[test]
fn the_conversion_saving_uses_the_measured_ratios() {
    let e = Env::new();
    // 2400 characters per 1000 tokens: 240000 html chars save 0.73 each, 120000 pdf chars 1.2 each
    let rec = r#"{"chars":420000,"day":"2026-11-16","docs":4,"fmt":{"chat":0,"html":2,"md":1,"pdf":1,"txt":0},"fmt_chars":{"chat":0,"html":240000,"md":60000,"pdf":120000,"txt":0},"id":"0123456789abcdef","lib_chars":420000,"lib_docs":4,"raw":420000,"src_bytes":9,"tk":2400,"v":2}"#;
    write(&e.usage_file(), &format!("{rec}\n"));
    let (_, out) = e.run(&["--usage"]);
    // (240000 * 0.73 + 120000 * 1.2) / 2400 = 133 thousand tokens; markdown has no saving
    assert!(out.contains("converted:     ~133k tokens"), "{out}");
    assert!(out.contains("1 of 1 loads say their formats"), "{out}");
}

// ---------------------------------------------------------------- opencode chats

#[cfg(unix)]
const OC1: &str = "ses_AbC123xyz";
#[cfg(unix)]
const OC2: &str = "ses_Def456uvw";

#[cfg(unix)]
fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// The sessions setup (no Claude chats) with a fake `opencode` program that answers
/// `session list` and `export ID` from files and logs every call. `updated` = ages in seconds.
#[cfg(unix)]
fn opencode_env(on: bool, ages: [u64; 2]) -> (Sess, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let s = sessions_env();
    let fake = s.e.tmp.join("fake-oc");
    fs::create_dir_all(&fake).unwrap();
    let t = now_ms();
    let list = serde_json::json!([
        {"id": OC1, "title": "Researching Rust", "updated": t - ages[0] * 1000, "created": t - 9_000_000, "directory": "/work/app"},
        {"id": OC2, "title": "New session - 2026-11-17T07:11:29.702Z", "updated": t - ages[1] * 1000, "created": t - 9_000_000, "directory": "/work/web"},
    ]);
    write(&fake.join("list.json"), &list.to_string());
    let msg = |role: &str, id: &str, parts: Value| serde_json::json!({"info": {"role": role, "id": id, "time": {"created": 1_794_899_706_762u64}}, "parts": parts});
    let ex1 = serde_json::json!({"info": {"id": OC1, "title": "Researching Rust"}, "messages": [
        msg("user", "m1", serde_json::json!([{"type": "text", "text": "Load the rust   research"}])),
        msg("assistant", "m2", serde_json::json!([
            {"type": "reasoning", "text": "hidden-thought"}, {"type": "text", "text": "Loaded four documents."},
            {"type": "tool", "tool": "bash", "state": {"output": "TOOLOUT"}},
            {"type": "text", "text": "synthetic-noise", "synthetic": true}])),
    ]});
    let ex2 = serde_json::json!({"info": {"id": OC2, "title": "New session - 2026-11-17T07:11:29.702Z"}, "messages": [
        msg("user", "m3", serde_json::json!([{"type": "text", "text": "Newest chat, other folder"}])),
    ]});
    write(&fake.join(format!("export-{OC1}.json")), &ex1.to_string());
    write(&fake.join(format!("export-{OC2}.json")), &ex2.to_string());
    let script = fake.join("opencode");
    write(
        &script,
        "#!/bin/sh\nd=\"$(dirname \"$0\")\"\necho \"$*\" >> \"$d/calls.log\"\ncase \"$1\" in\n  session) cat \"$d/list.json\" ;;\n  export) cat \"$d/export-$2.json\" ;;\nesac\n",
    );
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    if on {
        s.e.append_cfg(&format!("opencode = true\nopencode_cmd = \"{}\"\n", script.display()));
    }
    (s, fake)
}

#[cfg(unix)]
#[test]
fn an_empty_opencode_chat_is_exported_once() {
    let (s, fake) = opencode_env(true, [3600, 7200]);
    write(
        &fake.join(format!("export-{OC2}.json")),
        &serde_json::json!({"info": {"id": OC2}, "messages": []}).to_string(),
    );
    s.e.run(&["--sessions"]);
    s.e.run(&["--sessions"]);
    let calls = read(&fake.join("calls.log"));
    assert_eq!(calls.matches(&format!("export {OC2}")).count(), 1, "{calls}");
    assert_eq!(calls.matches(&format!("export {OC1}")).count(), 1, "{calls}");
}

#[cfg(unix)]
#[test]
fn opencode_chats_are_off_unless_asked() {
    let (s, fake) = opencode_env(false, [3600, 7200]);
    let (code, out) = s.e.run(&["--sessions"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("ses_"), "{out}");
    assert!(!fake.join("calls.log").exists());
}

#[cfg(unix)]
#[test]
fn opencode_chats_are_listed_loaded_and_cached() {
    let (s, fake) = opencode_env(true, [3600, 7200]);
    let (code, out) = s.e.run(&["--sessions"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("4 closed sessions") || out.contains("sessions tracked"), "{out}");
    assert!(out.contains(&format!("{OC1}  2026-11-")), "{out}");
    assert!(out.contains("Researching Rust"), "{out}");
    // the placeholder title is not a name: the first prompt is
    assert!(out.contains("Newest chat, other folder"), "{out}");
    // a lower-case prefix finds the mixed-case id and the full id is printed back
    let (code, out) = s.e.run(&["--session-load", "ses_abc1"]);
    assert_eq!(code, 0, "{out}");
    let path =
        out.lines().find_map(|l| l.split_once("chars  ").filter(|_| !l.contains("total")).map(|(_, p)| p)).unwrap();
    let text = read(Path::new(path));
    assert!(text.contains(&format!("SESSION ID: {OC1}")), "{text}");
    assert!(text.contains("USER: Load the rust   research"), "{text}");
    assert!(text.contains("ASSISTANT: Loaded four documents."), "{text}");
    for bad in ["hidden-thought", "TOOLOUT", "synthetic-noise"] {
        assert!(!text.contains(bad), "{bad}");
    }
    let (code, out) = s.e.run(&["--resume", "ses_abc1"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("cd /work/app && opencode -s {OC1}")), "{out}");
    // each chat was exported once, however often the list runs
    let calls = read(&fake.join("calls.log"));
    assert_eq!(calls.lines().filter(|l| l.starts_with("export")).count(), 2, "{calls}");
}

#[cfg(unix)]
#[test]
fn opencode_chats_join_the_context_picker() {
    let (s, _fake) = opencode_env(true, [3600, 7200]);
    let presets = s.e.conf_dir.join("contexts.json");
    save_preset(&presets, "work", &[OC1]);
    let (code, out) = s.e.run(&["--context", "work"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("CHAT \"Researching Rust\" ({OC1})")), "{out}");
    assert!(out.contains("total: 1 chats"), "{out}");
}

#[cfg(unix)]
#[test]
fn opencode_never_offers_the_chat_that_runs_this() {
    let (s, _fake) = opencode_env(true, [30, 7200]); // OC1 was written 30 s ago
    let presets = s.e.conf_dir.join("contexts.json");
    save_preset(&presets, "work", &[OC1, OC2]);
    let (_, out) = s.e.run_with(&["--context", "work"], &[("OPENCODE", "1")]);
    assert!(!out.contains(&format!("({OC1})")), "{out}");
    assert!(out.contains(&format!("({OC2})")), "{out}");
    assert!(out.contains(&format!("MISSING  chat {OC1}")), "{out}");
}

#[cfg(unix)]
#[test]
fn opencode_missing_program_is_said_not_fatal() {
    let (s, _fake) = opencode_env(false, [3600, 7200]);
    s.e.append_cfg("opencode = true\nopencode_cmd = \"/nonexistent/opencode\"\n");
    let (code, out) = s.e.run(&["--sessions"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("OPENCODE SKIPPED"), "{out}");
}
