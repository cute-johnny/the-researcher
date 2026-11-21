The five leftover findings of the 21 Nov review (issue #92, PR #93).

- **Clear config errors.** A config value of the wrong type (`sets`, `aliases`, `profiles`, a profile's `root` or `code`) gives `CONFIG ERROR` in the Python version instead of a traceback. A `code = "src/*.rs"` without brackets no longer lists the whole project. A profile `root` that is not text is a `CONFIG ERROR` in the Rust version too. A bad `idle_minutes` falls back to 10.
- **`/load-context` without the Rust program** finds the Python version next to the skill, so it works in an opencode-only install too.
- **Python version:** Today and Yesterday no longer shift by an hour after a clock change; `--usage` counts a damaged log line as 0 instead of crashing.
- **opencode:** an empty chat is exported once, not again on every load (about 2 s each).
- Linux x86_64 only for now; PDFs still need `pdftotext` (poppler-utils).

Install: download `researcher-v0.6.4-x86_64-linux`, check it against `SHA256SUMS.txt`, and put it on your PATH as `researcher`:

```bash
sha256sum -c SHA256SUMS.txt
install -m 755 researcher-v0.6.4-x86_64-linux ~/.local/bin/researcher
```

Or build it yourself with `cargo install --locked --path .`. Then install the skills as described in the README.
