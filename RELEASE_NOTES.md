The saving is counted against how you read research before (issue #102, PR #104). Same in the Rust and the Python version.

- **New `[usage] before = "pdf"` (or `"html"`):** set it with `researcher --usage-before pdf|html|none`. A loaded document that also exists in that format counts its conversion saving against it: a report loaded from its HTML copy counts 1.2 tokens per character (the PDF rate) instead of 0.73.
- Without the setting, every document counts against the file it was loaded from, the one copy you keep; research kept only as Markdown or text saves nothing there.
- Usage records are now version 3 (`vs_chars`: the characters per format they are counted against). The receiver takes versions 1 to 3; older lines keep counting the loaded format.
- The skill asks once how you gave Claude your research before, together with the sharing question.
- New header picture in the README (issue #103).
- Linux x86_64 only for now; PDFs still need `pdftotext` (poppler-utils).

Install: download `researcher-v0.7.0-x86_64-linux`, check it against `SHA256SUMS.txt`, and put it on your PATH as `researcher`:

```bash
sha256sum -c SHA256SUMS.txt
install -m 755 researcher-v0.7.0-x86_64-linux ~/.local/bin/researcher
```

Or build it yourself with `cargo install --locked --path .`. Then install the skills as described in the README.
