// SPDX-License-Identifier: MIT
//! Small helpers that behave like the Python standard library calls the original script used,
//! so both versions read and write the same files and produce the same text.

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

/// Python's `str.isspace()` for one character: Unicode White_Space plus \x1c-\x1f.
pub fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `str.rstrip()`.
pub fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_space)
}

/// `str.splitlines()` (no line ends kept, no empty last item after a final break).
pub fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if brk {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r'
                && let Some(&(j, '\n')) = it.peek()
            {
                it.next();
                end = j + 1;
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Number of characters (Python `len()` of a str).
pub fn chars(s: &str) -> usize {
    s.chars().count()
}

/// The first `n` characters of `s`.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The last `n` characters of `s`.
pub fn last_chars(s: &str, n: usize) -> &str {
    let total = chars(s);
    if n >= total {
        return s;
    }
    let (i, _) = s.char_indices().nth(total - n).expect("index inside the string");
    &s[i..]
}

/// Universal newlines, as Python's text mode reads a file: \r\n and \r become \n.
pub fn universal_newlines(s: &str) -> String {
    if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s.to_string() }
}

/// `os.path.expanduser` for `~` and `~/...` (not `~user`).
pub fn expanduser(p: &str) -> String {
    let home = || dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    if p == "~" {
        return home().unwrap_or_else(|| p.to_string());
    }
    for sep in ["~/", "~\\"] {
        if let Some(rest) = p.strip_prefix(sep)
            && let Some(h) = home()
        {
            return join(&h, rest);
        }
    }
    p.to_string()
}

/// `os.path.abspath`: absolute and lexically normalised (`.`, `..`, doubled separators).
pub fn abspath(p: &str) -> String {
    let path = Path::new(p);
    let full =
        if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut out = PathBuf::new();
    for c in full.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(out.components().next_back(), Some(Component::RootDir | Component::Prefix(_)) | None) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

/// A path for people to read: the home folder shown as `~`.
pub fn tilde(p: &str) -> String {
    if let Some(home) = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).filter(|h| h.len() > 1) {
        if p == home {
            return "~".into();
        }
        for sep in ['/', std::path::MAIN_SEPARATOR] {
            if let Some(rest) = p.strip_prefix(&format!("{home}{sep}")) {
                return format!("~{sep}{rest}");
            }
        }
    }
    p.to_string()
}

/// `os.path.abspath(os.path.expanduser(p))`.
pub fn user_path(p: &str) -> String {
    abspath(&expanduser(p))
}

/// `os.path.join(a, b)` on strings: one separator between the parts.
pub fn join(a: &str, b: &str) -> String {
    if a.is_empty() || a.ends_with('/') || a.ends_with(std::path::MAIN_SEPARATOR) {
        format!("{a}{b}")
    } else {
        format!("{a}{}{b}", std::path::MAIN_SEPARATOR)
    }
}

/// The folder part of a path string (`os.path.dirname`).
pub fn dirname(p: &str) -> String {
    Path::new(p).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Lower-case extension with its dot (`os.path.splitext(p)[1].lower()`), or "".
pub fn ext_lower(p: &str) -> String {
    let name = p.rsplit(['/', std::path::MAIN_SEPARATOR]).next().unwrap_or(p);
    split_ext(name).1.to_lowercase()
}

/// `os.path.splitext` of a file name: (stem, ".ext"); leading dots are not an extension.
pub fn split_ext(name: &str) -> (&str, &str) {
    let lead = name.len() - name.trim_start_matches('.').len();
    match name[lead..].rfind('.') {
        Some(i) => (&name[..lead + i], &name[lead + i..]),
        None => (name, ""),
    }
}

/// Write a file through a temporary name and a rename, so a reader never sees half of it. The
/// temporary name holds the process id: two runs at once never write into the same one.
pub fn write_atomic(path: &str, data: &[u8]) -> io::Result<()> {
    // a symlinked file (dotfiles) stays a link: write where it points, and keep its permissions
    let path = &fs::canonicalize(path).map_or_else(|_| path.to_string(), |p| p.to_string_lossy().into_owned());
    let perms = fs::metadata(path).ok().map(|m| m.permissions());
    let tmp = format!("{path}.{}.tmp", std::process::id());
    {
        let mut f = fs::File::create(&tmp)?;
        if let Err(e) = f.write_all(data) {
            drop(f);
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if let Some(p) = perms {
            let _ = f.set_permissions(p);
        }
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// A state file that exists but does not parse is kept as `PATH.bad` before the next save
/// overwrites it with an empty state.
pub fn set_aside(path: &str) {
    if fs::metadata(path).is_ok_and(|m| m.is_file()) && fs::rename(path, format!("{path}.bad")).is_ok() {
        eprintln!("NOTE: {path} could not be read; kept as {path}.bad, starting empty");
    }
}

/// `os.makedirs(dirname(path), exist_ok=True)`.
pub fn make_parent(path: &str) -> io::Result<()> {
    let d = dirname(path);
    if d.is_empty() { Ok(()) } else { fs::create_dir_all(d) }
}

/// A copy of `v` with every object's keys in sorted order (Python's `sort_keys=True`).
pub fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), sorted(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// JSON with a one-space indent, like Python's `json.dump(..., indent=1)`.
pub fn json_indent1(v: &Value) -> String {
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    serde::Serialize::serialize(v, &mut ser).expect("a JSON value always serialises");
    String::from_utf8(buf).expect("serde_json writes UTF-8")
}

/// Python truthiness of a JSON value (`if x:`).
pub fn truthy_json(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python truthiness of a TOML value.
pub fn truthy_toml(v: Option<&toml::Value>) -> bool {
    match v {
        None => false,
        Some(toml::Value::Boolean(b)) => *b,
        Some(toml::Value::Integer(i)) => *i != 0,
        Some(toml::Value::Float(f)) => *f != 0.0,
        Some(toml::Value::String(s)) => !s.is_empty(),
        Some(toml::Value::Array(a)) => !a.is_empty(),
        Some(toml::Value::Table(t)) => !t.is_empty(),
        Some(toml::Value::Datetime(_)) => true,
    }
}

/// Modification time in nanoseconds since the epoch (`st_mtime_ns`).
pub fn mtime_ns(meta: &fs::Metadata) -> i64 {
    use std::time::UNIX_EPOCH;
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i64,
            Err(e) => -(e.duration().as_nanos() as i64),
        },
        Err(_) => 0,
    }
}

/// Modification time in seconds as a float (`os.path.getmtime`).
pub fn mtime_secs(meta: &fs::Metadata) -> f64 {
    let ns = mtime_ns(meta);
    let (sec, nsec) = (ns.div_euclid(1_000_000_000), ns.rem_euclid(1_000_000_000));
    sec as f64 + nsec as f64 * 1e-9
}

/// `shlex.quote`.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_like_python() {
        assert_eq!(py_splitlines("a\nb\r\nc\rd\u{2028}e\n"), vec!["a", "b", "c", "d", "e"]);
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_splitlines("\n\n"), vec!["", ""]);
    }

    #[test]
    fn strip_includes_python_extra_spaces() {
        assert_eq!(py_strip("\u{1f} x\u{a0}"), "x");
    }

    #[test]
    fn splitext_like_python() {
        assert_eq!(split_ext("a.b.html"), ("a.b", ".html"));
        assert_eq!(split_ext(".hidden"), (".hidden", ""));
        assert_eq!(split_ext("noext"), ("noext", ""));
    }

    #[test]
    fn abspath_normalises() {
        #[cfg(unix)]
        {
            assert_eq!(abspath("/a/./b//c/../d"), "/a/b/d");
            assert_eq!(abspath("/.."), "/");
        }
        #[cfg(windows)]
        {
            assert_eq!(abspath(r"C:\a\.\b\\c\..\d"), r"C:\a\b\d");
            assert_eq!(abspath(r"C:\.."), r"C:\");
        }
    }

    #[test]
    fn quote_like_shlex() {
        assert_eq!(shell_quote("/work/app"), "/work/app");
        assert_eq!(shell_quote("/my dir/it's"), "'/my dir/it'\"'\"'s'");
    }

    #[test]
    fn char_slices() {
        assert_eq!(take_chars("héllo", 2), "hé");
        assert_eq!(last_chars("héllo", 4), "éllo");
    }
}
