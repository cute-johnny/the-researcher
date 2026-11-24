Chats moved away on `/clear` load again (issue #95, PR #96). Same in the Rust and the Python version.

- **New `[sessions] extra_dirs`:** folders with chats that were moved out of `~/.claude/projects` (a cleanup on `/clear`, an archive drive). Each is read as a folder of project folders (`<folder>/<project>/<id>.jsonl`) and as one folder per moved session (`<folder>/<anything>/projects/<project>/<id>.jsonl`).
- A chat found twice is taken from `projects_dir` first, then from the newest-named moved folder.
- A moved chat keeps its name and presets: the registry follows it to its new place on the next scan.
- Linux x86_64 only for now; PDFs still need `pdftotext` (poppler-utils).

Install: download `researcher-v0.6.5-x86_64-linux`, check it against `SHA256SUMS.txt`, and put it on your PATH as `researcher`:

```bash
sha256sum -c SHA256SUMS.txt
install -m 755 researcher-v0.6.5-x86_64-linux ~/.local/bin/researcher
```

Or build it yourself with `cargo install --locked --path .`. Then install the skills as described in the README.
