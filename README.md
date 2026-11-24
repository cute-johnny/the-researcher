# The Researcher

**A research loader for Claude.**

<p align="center"><img src="docs/img/header.jpg" alt="A white robot kneels in a field of grass and dandelions at sunset, looking at a dandelion clock in its hand" width="480"></p>

[![tokens squeezed](https://the-researcher-usage.ludalunizz.workers.dev/v1/badge/squeezed.svg)](#sharing-opt-in) [![tokens not loaded](https://the-researcher-usage.ludalunizz.workers.dev/v1/badge/not-loaded.svg)](#sharing-opt-in)   *so far, by all users*

New here? The [user manual](docs/MANUAL.md) walks through everything step by step.

Load your saved research into a [Claude Code](https://claude.com/claude-code) or [opencode](https://opencode.ai) session in one step, and only the part you need.

If you keep research notes, reports, or saved articles on disk and keep telling Claude "read my notes on X first", this does that for you. You name a **set** (`/researcher rust`), a small window lists the documents with their size in tokens, you tick what this session needs, and Claude reads exactly those. No tokens are spent while the window is open or on anything you leave unticked.

- **Sets** are named lists of documents in a small `config.toml`. Sets can include other sets, and ids can be globs (`rust/*`).
- **Saved defaults** per set: next time your last choice is pre-ticked, and documents added since are marked **NEW**.
- **Custom sets**: name a set you never defined (`/researcher side-project`) and the first call shows your whole library; what you tick becomes that set.
- **Formats**: `.html`, `.md`, `.txt`, and `.pdf` (through `pdftotext`). Folders with an `index.html` count as one document.
- **Read-only**: your library is never changed or moved. Claude reads a plain-text copy kept in the researcher's own folder (see [The text copy](#the-text-copy)).

## How it looks

The first picture is a set with a saved preset: the documents you ticked last time are ticked, a document added since is marked **NEW**, and the rest of your library is folded under "Add other research". The second is a set you have never used: your whole library, grouped by folder, with nothing ticked. Above the list are the time chips (Day | Week | Month | All) and "Group by: time | folder"; **rename** gives a document or a set a display name. **Cancel (load nothing)** always leaves without sending anything.

![Popup with a saved preset](docs/img/popup-saved-preset.png)

![Popup for a new set](docs/img/popup-new-set.png)

The first time you load research, before a library is set, a smaller window asks where your research is: the folder that holds it, or the folder where you will make or copy it from now on. Type a path or press **Choose folder…** for the system folder dialog. A folder that does not exist yet is made. The choice is saved as `library` in `config.toml`, and the chooser opens right after.

![First use: choose the research folder](docs/img/first-use.png)

`/load-context` (earlier chats as context, see [below](#earlier-chats-as-context-load-context)) opens the same window on its **Chats** tab; the **Research** tab next to it loads only when you click it (see [One window, two tabs](#one-window-two-tabs)). The first picture is a saved preset on the Week chip, grouped by time: the chats ticked last time are ticked, the rest are folded under "Add other chats", and chats started since are counted as newer and marked **NEW** once you unfold it. The second is a preset you have never used: the chats of this week under Today, Yesterday and Earlier this week, nothing ticked, with the preset's name field next to Remember.

![Chat window with a saved preset](docs/img/context-saved-preset.png)

![Chat window for a new preset](docs/img/context-new-preset.png)

`/researcher profile myapp gui` opens a [project profile](#project-profiles): Code | Chats | Research, with the preset buttons on top. The preset `gui` ticked two source files, two chats and one research document at once; Chats and Research loaded because the preset needed them.

![A project profile with the preset gui](docs/img/profile.png)

All pictures use the made-up documents in [examples/library](examples/library) and made-up chats. `tools/screenshots.sh` makes them again: it opens the windows six times for about a second each, needs a display, and runs with a throwaway home folder, so none of your own files, settings or paths are touched or shown.

## The text copy

Every document of your library gets one plain-text file in the researcher's own folder, `~/.local/share/researcher/text/` (or `text_dir` in `config.toml`). Your own files are only read.

- Made **once** per document, and again only when the source file changes (size or modified time) or the researcher's converter changes. Opening the popup on a library you already converted costs about a tenth of a second instead of a `pdftotext` run per PDF.
- `researcher --sync` makes or refreshes the copy of the whole library and loads nothing.
- Copies stay when a document leaves the library (a library on an unplugged drive keeps working). Delete the folder any time; it is rebuilt.
- HTML and Markdown comments (`<!-- ... -->`) are dropped before anything else, so text hidden in a comment never reaches Claude. Hidden elements (`display:none`, `<details>`) are still kept.
- The text is squeezed for tokens, without dropping content: PDF layout padding and indentation, page numbers and headers/footers that repeat on most pages, lines of rule characters, zero-width characters, runs of blank lines and exact repeated lines (PDF and HTML only). Markdown and text files are only tidied (indentation, code and repeated lines are kept).
- Measured in characters on 11 sampled PDFs (16 Nov 2026): 394,359 characters from `pdftotext -layout`, 251,104 after squeezing, so **36% fewer**; per document 2% to 48%. The spread follows how much of a PDF is padding. Tokens were not counted for this step. A library that mostly holds HTML gains little, because HTML is already converted to text.

## Usage log

After every load the researcher appends one line to `~/.local/share/researcher/usage.jsonl`, so you can see what it saves on **your** library. A load is a library load (`researcher SET`) or earlier chats loaded as context (`--context`, `--session-load`); listing, setting up and cancelling load nothing and log nothing. It is local and anonymous, and **nothing is sent unless you say yes** (see Sharing below).

What a line holds, and nothing else: the day, a random install id (`usage-id` next to the log; delete it to start a new one), how many documents were loaded and in which formats (pdf, html, md, txt, or `chat` for a chat loaded as context), and character sums: source bytes, converted text before and after squeezing, the loaded characters per format, and the whole library (for a chat load: every chat up to the newest one picked) after squeezing. It never holds a title, a path, a set name or where your library is. The log is plain JSON lines; read it before you share anything.

```
researcher --usage                 totals: characters loaded, squeezed away, left unticked, with estimated tokens
researcher --usage-export [FILE]   write the records to FILE (default researcher-usage.json) so you can look at them
```

- **Squeezed away** is real: converted text before minus after the squeeze, for the documents you loaded.
- **Converted** is the saving from reading text instead of the original: an HTML page costs 1.73x its converted text and a PDF 2.2x (measured below), so each loaded HTML character saves 0.73 and each PDF character 1.2 of a text character. Markdown, text files and chats are read as they are and save nothing here. Older log lines (before 0.5) do not say their formats and add nothing to this number.
- **Left unticked** compares each load with loading your whole library; it is a ceiling, not a measurement, because nobody would load everything. For earlier chats the comparison is every past chat up to the newest one you picked (last active at or before it), minus the ones you loaded: what catching up on all of them would have cost.
- Tokens are estimated from characters (2.4 per token, see below), not counted by a tokenizer.
- Switch the log off with `[usage]` `log = false` in `config.toml`; then no log and no install id are written.

### Sharing (opt-in)

You can send the same anonymous lines to the project, so it can see what the researcher saves across installs. It is **off until you say yes**: while you have not decided, a load ends with `USAGE SHARING: not decided` and the skill asks you once.

```
researcher --usage-share on        send the lines (saved as `share = true` under [usage] in config.toml)
researcher --usage-share off       never send (`share = false`)
researcher --usage-send --dry-run  print exactly what would be sent, send nothing
researcher --usage-send            send the lines not sent yet, now
```

- With sharing on, each load starts a sender in the background: the load never waits on the network, and a failed send is tried again after the next load.
- What is sent is the log lines as they are, plus `seq`: the line's number in your log, so two identical loads on one day both count and a line sent twice is stored once. Nothing else: no title, path, set name, library location, GitHub login or account. The receiver ([`worker/`](worker/)) refuses any other field and keeps no IP address, headers or request logs (Cloudflare still sees the connection, as any host does).
- The two badges at the top are live totals over every shared load: **tokens squeezed** is the squeeze (the converted text before minus after squeezing) plus the conversion saving above (reading text instead of the HTML or PDF), both from the ratios measured on 85 reports; **tokens not loaded** compares each load with loading the whole library every time, so it is a ceiling, not a measurement (for a chat load: every past chat up to the newest one picked). Both are estimated from characters with the ratio each load used (2.4 per token). A new load shows up on this page after about 5 to 10 minutes (the badge images are cached for 5 minutes). Raw totals: [`/v1/stats`](https://the-researcher-usage.ludalunizz.workers.dev/v1/stats).
- `researcher --usage` ends with the sharing state and how many lines wait. `usage-sent` next to the log holds how far the log was sent.
- `[usage] url = "..."` sends to your own receiver instead (the same `worker/` deployed on your account).
- To have your lines removed from the project's receiver, open an issue with your install id (the `usage-id` file); then delete `usage-id` to start a new one.

## What it saves

Measured with Anthropic's `count_tokens` endpoint (model `claude-sonnet-5-5`, 16 Nov 2026) on 85 reports from one library, about 1.29M tokens of HTML in total. Each was counted as the raw HTML and as the text the researcher converts it to. For the first 50 (the ones that also exist as PDF) the PDF was counted too. The 85 are the library's reports that have an HTML version, apart from duplicate copies of the same title and a layout test page (nine of them Dutch).

| | Raw HTML | Converted text | PDF (first 50 only) |
|---|---:|---:|---:|
| Total, 85 documents | 1,289,597 | 745,528 | |
| Average per document | 15,172 | 8,771 | |
| Total, first 50 documents | 755,968 | 443,537 | 975,533 |
| Average per document, first 50 | 15,119 | 8,871 | 19,511 |
| Relative to the converted text | 1.7x | 1x | 2.2x |

- Converting HTML to text cuts the load to **57.8%** of the raw HTML (42% saved). Per document it ranged from 34% to 83% (median 58%): pages heavy in markup, tables and inline CSS gain most.
- A PDF read as a document costs **2.2x** the converted text (1.9x to 2.8x per document, median 2.3x, measured on the first 50), because the page images are counted as well.
- The researcher still reads every ticked document in full. The larger saving is what you leave unticked, which costs nothing. It also replaces the search-and-read turns of "read my notes on X" with one call.
- The converted text came to **2.4 characters per token** over the 85 documents (2.0 to 3.0 per document, token-weighted 2.42), and the popup sizes and the usage log assume **2.4**. An earlier flat 4 characters per token understated the real cost by roughly 1.7x. The ratio varies with the content: code, tables and non-English text tokenize denser than prose (the nine Dutch documents averaged 2.05, the others 2.5), so treat popup sizes as close, not exact.

### Keep research out of `MEMORY.md`

Claude Code loads `MEMORY.md` (and `CLAUDE.md`) in full at the start of every session. If it lists a pointer to every research document you own, you pay for all of those lines in every session, including the ones that never touch the topic. Do not do that. Keep at most one line in `MEMORY.md` that says research lives in the library and is loaded with `/researcher SET`, and let the window pick what a session needs. The pointers then cost nothing until you load them.

Eighty-five documents from one person's library is still a sample of one topic mix, so read the percentages as indicative.

## Requirements

- Linux x86_64 with a desktop (Wayland or X11) for the popup. Without a display it uses your saved defaults.
- For PDFs: `pdftotext` from poppler (`poppler-utils`)

The program is written in Rust (`src/`) and the popup in egui, so nothing else is needed. Windows and macOS builds are checked in CI but not supported yet; on those systems the config and data folders will be the native ones (for example `%APPDATA%\researcher` on Windows).

The original Python version, `skills/researcher/researcher.py`, still works and is kept as a fallback. It needs Python 3.11 or newer, and for its popup GTK 4 and PyGObject (`python3-gi`, `gir1.2-gtk-4.0` on Debian/Ubuntu). Both versions use the same files and write the same text copies (a test checks this byte for byte), so you can switch at any time.

## Install

1. The program: download `researcher-<version>-x86_64-linux` and `SHA256SUMS.txt` from the [latest release](https://github.com/LudaLunizz/the-researcher/releases/latest), check and install it:
   ```bash
   sha256sum -c SHA256SUMS.txt
   install -m 755 researcher-*-x86_64-linux ~/.local/bin/researcher     # any folder on your PATH
   ```
   Or build it from the clone: `cargo install --locked --path .`
2. The skill, and a starter config:
   ```bash
   git clone https://github.com/LudaLunizz/the-researcher.git
   cd the-researcher
   mkdir -p ~/.claude/skills
   cp -r skills/researcher ~/.claude/skills/        # or: ln -s "$PWD/skills/researcher" ~/.claude/skills/researcher
   cp -r skills/load-context ~/.claude/skills/      # optional: /load-context for earlier chats

   researcher --init ~/research                     # optional: writes ~/.config/researcher/config.toml
   ```

The skill runs `researcher` when it is on your PATH, and `researcher.py` otherwise. You can skip `--init`: on the first load the Rust version asks for your research folder in a window and writes the config itself.

### In opencode

The same skill works in [opencode](https://opencode.ai). opencode reads skills from `~/.config/opencode/skills/` and slash commands from `~/.config/opencode/command/`:

```bash
mkdir -p ~/.config/opencode/skills ~/.config/opencode/command
ln -s "$PWD/skills/researcher" ~/.config/opencode/skills/researcher
cp opencode/commands/researcher.md ~/.config/opencode/command/researcher.md
```

Restart opencode, then type `/researcher rust`, or say "load my research for rust". It runs the same `researcher` program with the same config, library, text copy and usage log, so Claude Code and opencode share everything. If opencode runs sandboxed (a container, bubblewrap) it needs read access to the program, `~/.config/researcher`, your library and the text copy, and a display for the popup; without a display you get the saved defaults. opencode also reads `~/.claude/skills/` on its own unless `OPENCODE_DISABLE_CLAUDE_CODE` is set, so the Claude Code install above may already be enough.

opencode asks before reading files outside the project folder, and the loader prints paths in the researcher's own folders. To skip that question for just those two folders, add this to `~/.config/opencode/opencode.jsonc`:

```jsonc
"permission": { "external_directory": { "~/.cache/researcher/*": "allow", "~/.local/share/researcher/text/*": "allow" } }
```

The popup title names the program that asked: opencode when opencode started the loader (it sets `OPENCODE=1`), Claude Code otherwise. `RESEARCHER_HOST="name"` overrides it.

`/load-context` works in opencode too: copy `opencode/commands/load-context.md` next to the other command, link `skills/load-context`, and switch on opencode chats in the config (see Sessions, below). Without that, `--sessions` and `--context` only know Claude Code's chats.

Try it first on the made-up documents in [examples/library](examples/library): `researcher --library examples/library rust`. Then edit `~/.config/researcher/config.toml` (see [examples/config.toml](examples/config.toml)) and in Claude Code type `/researcher rust`, or just say "load my research for rust".

## Configure

```toml
library = "~/research"        # folder with your documents
default_set = "rust"          # used when you name no set
exclude = ["scratch/*"]       # ids left out of the library (globs)

[aliases]
rs = "rust"

[sets.rust]                   # a topic
docs = ["rust/linux-notes", "rust/memory-*"]

[sets.myapp]                  # a project: other sets plus its own documents
include = ["rust"]
docs = ["myapp/roadmap"]
```

A document id is its path inside the library without the extension: `~/research/rust/linux-notes.html` is `rust/linux-notes`. If `x.html` and `x.pdf` both exist, the HTML one is used. Hidden files and folders are skipped. `all` is a built-in set with the whole library.

`RESEARCHER_LIBRARY` or `--library PATH` overrides the library for one run.

## Command line

```
researcher [SET ...]              popup, then convert the ticked documents and print their paths
researcher --setup SET            popup only: change the saved defaults, load nothing
researcher --no-popup SET         use the saved defaults without asking
researcher --list                 sets, document counts, saved defaults
researcher --sync                 make or refresh the text copy of the whole library, load nothing
researcher --usage                local totals of what it saved (see Usage log)
researcher --usage-export [FILE]  write the anonymous usage records to a file; nothing is sent
researcher --usage-share on|off   share the anonymous usage counts with the project, or not
researcher --usage-send [--dry-run]  send the usage records not sent yet (only with sharing on)
researcher --init [LIBRARY]       write a starter config
researcher --sessions             list tracked closed sessions (opt-in, see Sessions)
researcher --rename ID NAME       rename a tracked session ("" = back to the automatic name)
researcher --resume ID            print the command that resumes a session
researcher --session-load ID      write a session's conversation as a .txt to load as context
researcher --context [PRESET ...] pick earlier chats in a popup and load them (see Earlier chats)
researcher --start code|chats|research  the tab the window opens on (see One window, two tabs)
researcher --profile NAME [PRESET]  code, chats and research of a project, with presets (see Project profiles)
researcher --context --setup P    change a context preset, load nothing
researcher --context --list       list the saved context presets
```

It prints one path per ticked document; the skill tells Claude to read each one in full. Exit codes: 0 ok, 2 unknown set, 3 cancelled, 4 popup timed out, 5 custom set with no saved choice and no popup, 6 config or library problem, 7 sessions switched off, 8 session id not found or ambiguous, 9 usage sharing not switched on (`--usage-send`), 1 a send failed (the records stay queued).

Files: config, saved choices, the session registry and context presets in `~/.config/researcher/` (`config.toml`, `choices.json`, `sessions.json`, `contexts.json`, display names in `names.json`, the last time chip and grouping in `ui.json`), the text copy in `~/.local/share/researcher/text/` (`.index.json` there tracks which source each copy came from), the usage log (`usage.jsonl`, `usage-id`, `usage-sent`) in `~/.local/share/researcher/`, and session loads in `~/.cache/researcher/`.

## Sessions (optional)

Off by default. Switch it on in `config.toml`:

```toml
[sessions]
enabled = true
# idle_minutes = 10                  # a session untouched this long counts as closed
# projects_dir = "~/.claude/projects"
# extra_dirs = ["~/old-chats"]       # more folders with moved chats (see below)
# opencode = true                    # also list opencode's chats (asks the opencode program)
# opencode_cmd = "opencode"          # the program to ask, when it is not on PATH
```

Then `researcher --sessions` lists every closed Claude Code (and, if switched on, opencode) session, newest first: the **full session id** (the one Claude Code prints when you close a session), the last-active time and a name. The name comes from Claude Code's own title if the session has one, otherwise from the first prompt. `--rename ID NAME` replaces it, and a rescan never overwrites a name you set.

Name and id are stored together, keyed by the full id, and every command prints them together (`"name" (id)`), so a name can't drift onto the wrong session. `ID` may be a unique prefix of 4 or more characters. `--resume ID` prints `cd <project folder> && claude --resume <id>` (Claude Code only finds a session from its own folder). `--session-load ID` writes the session's prompts and replies, without tool output, as a `.txt`; the skill reads it like a research document. Sessions longer than 200,000 characters keep their start and their end.

**opencode chats.** opencode keeps its chats in its own database, so with `opencode = true` the researcher asks the program itself (`opencode session list`, `opencode export ID`) and keeps one plain transcript per chat in `sessions-opencode/` next to `sessions.json`. Each chat is exported once and again only after it changed; the first run takes a few seconds per chat (four at a time). They show up in `--sessions`, `--context` and the other commands like any chat, with opencode's own title as the name (the first prompt when it has none) and the folder opencode ran in. `--resume ID` prints `cd <folder> && opencode -s <id>`. Only top-level chats are listed, not the helper sessions a sub-agent opens. Inside opencode (it sets `OPENCODE=1`) the chat you type `/load-context` in is left out. If the program cannot be run, the output says `OPENCODE SKIPPED (...)` and Claude Code's chats still load. Set `opencode_cmd` to the full path when opencode is not on `PATH` (a sandbox, say); the program must see the same opencode data as your chats.

**Moved chats.** If something moves closed chats out of `~/.claude/projects` (a cleanup on `/clear`, an archive drive), list those folders in `extra_dirs`. Each one may hold project folders like `~/.claude/projects` does (`<folder>/<project>/<id>.jsonl`), or one folder per moved session that holds a `projects` folder (`<folder>/<anything>/projects/<project>/<id>.jsonl`); both are read. A chat found in more than one place is taken from `projects_dir` first, then from the newest-named moved folder. A chat that moved keeps its name and presets: the registry follows it to its new place on the next scan.

The registry only reads `~/.claude/projects` (plus `extra_dirs`) (and asks opencode for its chats); it never changes your transcripts. Sessions are loaded as reference material, the same caution as the documents below applies.

## Earlier chats as context (`/load-context`)

With sessions switched on, `/load-context` in Claude Code (or `researcher --context`) opens the same kind of window as a research set, but it lists your earlier chats: every project folder, the most recently used folder first, newest chat first. Each chat shows its **first prompt** (or the name you gave it with `--rename`), its size in tokens and the day it was last active. A filter box finds a chat by words from its first prompt or its folder. The chat you type `/load-context` in is never listed; chats you closed a minute ago are.

Pictures of this window are under [How it looks](#how-it-looks).

Presets work like custom sets. `/load-context noname` the first time shows every chat unticked; tick what belongs to noname and keep "Remember" ticked, and the next `/load-context noname` starts with those ticked. Chats added since show NEW and are never ticked on their own, and Remember is only pre-ticked the first time, so a one-off pick does not replace the preset. Without a name the preset is `default`. `--context --setup NAME` changes a preset, `--context --list` shows them, `--no-popup` loads a saved preset without asking.

Each ticked chat is written like `--session-load` (prompts and replies, no tool traffic) to `~/.cache/researcher/`, and the skill reads them. Presets live in `~/.config/researcher/contexts.json`.

Install the skill next to `researcher`: `cp -r skills/load-context ~/.claude/skills/` (or `ln -s "$PWD/skills/load-context" ~/.claude/skills/load-context`).

## One window, two tabs

When sessions are switched on, every load opens one window with two tabs: **Chats | Research**. `researcher SET` (and `/researcher`) opens on Research, `researcher --context` (and `/load-context`) opens on Chats, and `--start chats|research` picks the tab yourself. The other tab holds your preset `default` (chats) or the config's `default_set` (research), but it loads nothing until you click it: then it shows "loading…" while it scans (chats can take a few seconds the first time), so a load you only want one side of never waits for the other. Once open it has its saved ticks; a set or preset you never saved starts unticked. Each tab keeps its own ticks, filter and presets, and one **Load** button loads what is ticked in both ("Load 3 chats + 5 docs"). The tab the window opened on prints first. Without a window (`--no-popup`, no display) there is no tab to click, so only the command's own side loads. A load of both writes two usage records, one for the documents and one for the chats.

**Time chips.** Above the list: **Day | Week | Month | All** (the last 24 hours, 7 days, 30 days), each with its count. Chats go by when they were last active, documents by when their file last changed. **Group by: time** puts the rows under Today, Yesterday, Earlier this week, Earlier this month and Older, newest first, with the project folder as a small label on each chat; **folder** is the old view. A ticked row the chip hides keeps its tick and is still loaded; the line above the buttons says how many. The window remembers the chip and the grouping per tab in `ui.json` (first time: chats on Week by time, research on All by folder).

**Names.** **rename** next to a row (or a double-click on it) renames it in place; Enter keeps the name, Esc undoes it.
- A chat's name goes into `sessions.json`, the same as `--rename`; an empty name gives it its first prompt back.
- A document's name and a research set's name (**rename** next to the set) go into `names.json`; your files are never touched. The window shows the name with the real id in small text, the filter finds both, and the output gets a `DOC "name" (id)` line before the document's path.
- A chat preset has a name field next to **Remember**: a new name moves the saved ticks to it. Lowercase letters, digits, `.`, `_` and `-`, and not the name of another preset.

## Project profiles

A profile puts a project's source code next to its chats and research, with presets that tick all three at once: say one for the full source, one for the GUI and one for the libraries, each with its own chats and research. Set it up in `config.toml`:

```toml
[profiles.myapp]
root = "~/src/myapp"          # the project folder (optional: without it there is no Code tab)
code = ["src/*", "*.toml"]    # globs over paths inside root; `*` crosses folders (default: "*")
```

`researcher --profile myapp` opens one window with **Code | Chats | Research** and a row of preset buttons. Code is ready at once; Chats and Research load when you click their tab or a preset that needs them. Clicking a preset ticks its files, chats and documents (you can still change the ticks); tick **Save these ticks as the profile preset**, give it a name and press Load to keep them (an existing name is replaced). Presets live in `profiles.json` next to the config. `researcher --profile myapp gui` opens with the preset `gui` ticked, `--no-popup` loads it without a window, `--profile myapp --list` shows the presets.

Source files are printed as `CODE <path in the project>` followed by their size and full path; Claude reads them from disk, so there is no text copy, and code is not counted in the usage log. Hidden files and folders, `target`, `node_modules`, `__pycache__`, `dist`, `build`, `venv`, symlinks, binaries and files over 1 MB are left out. A profile saves no set defaults or chat presets of its own.

## Safety

Documents are loaded into Claude's context as text. Treat them like any untrusted input: a saved web page can contain instructions aimed at an AI. The skill tells Claude to treat document contents as reference material, not as commands, but only load documents you trust. HTML and Markdown comments are removed when a document is converted; other hidden content (`display:none`, collapsed `<details>`, white text in a PDF) is not.

## Tests

```bash
python3 -I -m unittest discover -s tests -v
cargo test                                  # the Rust version, plus the check that both write the same text
```

The tests cover the library scan, set expansion, saved defaults, custom sets, and the command line. The popup needs a display and is not covered by CI. It only opens when the script is run for a load or `--setup`; set `RESEARCHER_NO_POPUP=1` to make that impossible (scripts, CI).

## License

MIT License, see [LICENSE](LICENSE). Your own documents and config are never part of this repository and are not covered by it.
