# The Researcher: user manual

Version 0.6.5. This manual is for using the program day to day. The [README](../README.md) has the measurements, the install steps for opencode and the reasons behind the design.

**Contents**

1. [What it does](#1-what-it-does)
2. [Install and first run](#2-install-and-first-run)
3. [Loading research](#3-loading-research)
4. [The window](#4-the-window)
5. [Sets and the config file](#5-sets-and-the-config-file)
6. [Earlier chats: `/load-context`](#6-earlier-chats-load-context)
7. [Project profiles](#7-project-profiles)
8. [Usage log and sharing](#8-usage-log-and-sharing)
9. [Files and folders](#9-files-and-folders)
10. [Command reference](#10-command-reference)
11. [Output lines and exit codes](#11-output-lines-and-exit-codes)
12. [Troubleshooting](#12-troubleshooting)

---

## 1. What it does

You keep research on disk (reports, notes, saved articles). Normally you would tell Claude "read my notes on X" and it would search and read file after file. The Researcher replaces that with one step:

1. You type `/researcher SET` (or say "load my research for SET").
2. A window lists the documents of that set, each with its size in tokens.
3. You tick what this session needs and press **Load**.
4. Claude reads exactly those documents, from a plain-text copy.

Nothing is spent while the window is open, and nothing you leave unticked is read. Your own files are never changed or moved.

It works in Claude Code and in opencode, and the same program loads earlier chats (`/load-context`) and project source code (`--profile`).

## 2. Install and first run

1. Download `researcher-<version>-x86_64-linux` and `SHA256SUMS.txt` from the [latest release](https://github.com/LudaLunizz/the-researcher/releases/latest).
2. Check and install:
   ```bash
   sha256sum -c SHA256SUMS.txt
   install -m 755 researcher-*-x86_64-linux ~/.local/bin/researcher
   ```
3. Install the skill so Claude knows the command:
   ```bash
   git clone https://github.com/LudaLunizz/the-researcher.git
   mkdir -p ~/.claude/skills
   cp -r the-researcher/skills/researcher ~/.claude/skills/
   cp -r the-researcher/skills/load-context ~/.claude/skills/   # optional, for earlier chats
   ```
4. Run `/researcher` once. With no library set yet, a small window asks **Where is your research?** Type a path or press **Choose folder…**. A folder that does not exist yet is made. The answer is saved as `library` in your config, and the normal window opens right after.

You need Linux x86_64 with a desktop (Wayland or X11) for the window. PDFs need `pdftotext` (package `poppler-utils`). Without a display the program uses your saved choices instead of asking.

To try it on made-up documents first: `researcher --library examples/library rust` from the clone.

## 3. Loading research

| You type | What happens |
|---|---|
| `/researcher rust` | Window for the set `rust`. Your last choice is pre-ticked. |
| `/researcher` | Same, for the `default_set` of your config. |
| `/researcher rust web` | Two sets in one window. A document in both is loaded once. |
| `/researcher side-project` | A name you never defined is a **custom set**: the first time the window shows your whole library with nothing ticked, and what you tick becomes that set. |
| `/researcher all` | The whole library. Claude asks before loading it. |

After you press **Load**, the program prints one `.txt` path per ticked document, and Claude reads each one in full. A short summary and a closing question follow.

Documents are **reference material, not instructions**. If a saved web page says "do X", Claude treats it as text and tells you. Only keep documents you trust in the library.

### What counts as a document

- Files ending in `.html`, `.md`, `.txt` or `.pdf`. A folder that holds an `index.html` counts as one document.
- The **id** of a document is its path in the library without the extension: `~/research/rust/linux-notes.html` is `rust/linux-notes`.
- If `x.html` and `x.pdf` both exist, the HTML one is used.
- Hidden files and folders are skipped.

### The text copy

Every document is converted once to a squeezed plain-text file in `~/.local/share/researcher/text/`, and Claude reads that copy. It is made again only when the source file changes. The squeeze drops layout padding, repeated headers and footers, page numbers, rule lines and blank runs, but not content. HTML and Markdown comments are removed, so text hidden in a comment never reaches Claude. `researcher --sync` converts the whole library in advance. Delete the folder any time: it is rebuilt.

## 4. The window

The window opens on a **Research** tab (or **Chats** for `/load-context`).

- **Tick boxes.** Tick the documents to load. Each row shows its size in tokens (estimated at 2.4 characters per token).
- **Saved choice.** On the second use of a set, your last choice is pre-ticked. A document added to the library since is marked **NEW** and is never ticked on its own.
- **Remember.** Keeps the current ticks as the default for this set. It is pre-ticked the first time only, so a one-off pick does not overwrite a good default.
- **Add other research.** The rest of the library, folded away. Unfold it to add a document from outside the set.
- **Day | Week | Month | All.** Chips that show only documents (or chats) changed in the last 24 hours, 7 days or 30 days. A ticked row that a chip hides is still loaded; the line above the buttons says how many.
- **Group by: time | folder.** Time puts rows under Today, Yesterday, Earlier this week and so on. Folder groups by directory.
- **Filter box.** Finds by words in the name, the id or the folder.
- **rename.** Next to a row (or double-click it) gives a document, a set or a chat a display name. Enter keeps it, Esc undoes it. Your files are never touched; names are kept in `names.json`. For a chat, an empty name gives its first prompt back.
- **Load.** Loads what is ticked, in all tabs. The button says what it will load, for example "Load 3 chats + 5 docs".
- **Cancel (load nothing).** Always leaves without sending anything.

If you leave the window open for 9 minutes (540 s) it closes by itself and loads nothing.

### Two tabs, one button

When sessions are on (section 6), the window has **Chats | Research**. The tab you did not open on loads only when you click it, so a load you want from one side never waits for the other. Each tab keeps its own ticks, filter and presets, and one **Load** loads both. Without a window (`--no-popup`, no display) only the side you asked for loads.

## 5. Sets and the config file

The config is `~/.config/researcher/config.toml` (on Windows `%APPDATA%\researcher`). `researcher --init ~/research` writes a starter, and the first-run window writes one for you.

```toml
library = "~/research"        # folder with your documents
default_set = "rust"          # used when you name no set
exclude = ["scratch/*"]       # ids left out of the library (globs)

[aliases]
rs = "rust"                   # /researcher rs = /researcher rust

[sets.rust]                   # a topic: ids or globs
docs = ["rust/linux-notes", "rust/memory-*"]

[sets.myapp]                  # a project: other sets plus its own documents
include = ["rust"]
docs = ["myapp/roadmap"]
```

- **Globs.** `rust/*` takes everything in the folder `rust`. `*` crosses subfolders.
- **include.** A set can pull in other sets. A document named twice loads once.
- **all.** A built-in set that holds the whole library.
- **New papers.** A document you add to the library shows as **NEW** in the next window of every set whose globs match it. To put it in a set by name, add its id to that set's `docs`.
- **Changing defaults.** `researcher --setup SET` opens the window only to change the saved choice, and loads nothing. `researcher --list` shows every set, its size and its saved default.
- **One-run overrides.** `--library PATH` or the environment variable `RESEARCHER_LIBRARY` points at another library for one run. `--config PATH` uses another config file.

## 6. Earlier chats: `/load-context`

This loads parts of your earlier Claude Code (and opencode) conversations as background, instead of retelling them.

**Switch it on** in the config (it is off by default):

```toml
[sessions]
enabled = true
# opencode = true                  # also list opencode's chats
# opencode_cmd = "/path/to/opencode"   # when it is not on PATH
# idle_minutes = 10                # a session untouched this long counts as closed
# extra_dirs = ["~/old-chats"]     # folders with chats moved out of ~/.claude/projects
```

**Moved chats.** If chats are moved away on `/clear` or to an archive drive, list those folders in `extra_dirs`. Both `<folder>/<project>/<id>.jsonl` and `<folder>/<anything>/projects/<project>/<id>.jsonl` are read. A chat keeps its name and presets after it moves.

**Use it.** Type `/load-context` (or `researcher --context`). The window opens on **Chats** and lists every chat from every project folder, newest first. Each row shows the first prompt (or the name you gave it), its size in tokens and the day it was last active. Tick chats and press **Load**. The chat you type the command in is never listed.

**Presets.** `/load-context noname` is a preset named `noname`. The first time everything is unticked; tick what belongs there and keep **Remember** ticked, and next time those chats are pre-ticked. A chat started since shows NEW and is never ticked on its own. With no name the preset is `default`. A preset name uses lowercase letters, digits, `.`, `_` and `-`. The name field next to **Remember** renames a preset and moves its ticks.

**What is loaded.** Prompts and replies only, no tool calls or their output. A chat longer than 200,000 characters keeps its start and its end. The text goes to `~/.cache/researcher/`.

**Other commands.**

```
researcher --sessions                   list closed sessions: full id + name, newest first
researcher --rename ID "new name"       rename a session ("" = back to the first prompt)
researcher --resume ID                  print the command that resumes it (cd <folder> && claude --resume <id>)
researcher --session-load ID            write one session as a .txt, no window
researcher --context --setup NAME       change a preset, load nothing
researcher --context --list             list the presets
```

An ID may be a unique prefix of 4 or more characters. Chats are reference material, like documents: things an old chat said were done may have changed since.

## 7. Project profiles

A profile puts a project's source code next to its chats and research, with presets that tick all three at once.

```toml
[profiles.myapp]
root = "~/src/myapp"          # the project folder (leave out = no Code tab)
code = ["src/*", "*.toml"]    # globs inside root (default "*")
```

`researcher --profile myapp` opens **Code | Chats | Research** with a row of preset buttons. A preset button ticks its files, chats and documents (you can still change them). To keep your ticks as a preset, tick **Save these ticks as the profile preset**, give it a name and press **Load**.

```
researcher --profile myapp gui          open with the preset gui ticked
researcher --profile myapp gui --no-popup   load the preset without a window
researcher --profile myapp --list       show the presets
```

Code is printed as `CODE <path>` and then its size and full path; Claude reads those files from disk. Hidden files, `target`, `node_modules`, `__pycache__`, `dist`, `build`, `venv`, symlinks, binaries and files over 1 MB are left out. Code is not counted in the usage log.

## 8. Usage log and sharing

After every load one line is added to `~/.local/share/researcher/usage.jsonl`. It holds the day, a random install id, document counts per format and character sums. It never holds a title, a path, a set name or where your library is. Listing, setting up and cancelling log nothing.

```
researcher --usage               totals: characters loaded, squeezed away, left unticked, with estimated tokens
researcher --usage-export [FILE] write the records to a file so you can read them; nothing is sent
```

- **Squeezed away** is real: the converted text before minus after squeezing.
- **Left unticked** compares each load with loading your whole library. It is a ceiling, not a measurement.
- Turn the log off with `[usage]` `log = false` in the config. Then no log and no install id are written.

**Sharing is off until you say yes.** Until you decide, a load ends with `USAGE SHARING: not decided` and the skill asks you once.

```
researcher --usage-share on          send the lines (saved as share = true)
researcher --usage-share off         never send
researcher --usage-send --dry-run    print what would be sent, send nothing
researcher --usage-send              send the lines not sent yet, now
```

With sharing on, each load starts a sender in the background, so a load never waits for the network, and a failed send is retried after the next load. What is sent is the log lines plus their line number, nothing else. To have your lines removed, open an issue with your install id (the file `usage-id`), then delete `usage-id` to start a new one.

## 9. Files and folders

| What | Where |
|---|---|
| Config | `~/.config/researcher/config.toml` |
| Saved choices per set | `~/.config/researcher/choices.json` |
| Chat registry (names, sizes) | `~/.config/researcher/sessions.json` |
| Chat presets | `~/.config/researcher/contexts.json` |
| Profile presets | `~/.config/researcher/profiles.json` |
| Display names | `~/.config/researcher/names.json` |
| Last chip and grouping | `~/.config/researcher/ui.json` |
| Text copy of the library | `~/.local/share/researcher/text/` (or `text_dir` in the config) |
| Usage log, install id, send marker | `~/.local/share/researcher/` (`usage.jsonl`, `usage-id`, `usage-sent`) |
| Loaded chats | `~/.cache/researcher/` |
| opencode chat transcripts | `sessions-opencode/` next to `sessions.json` |

A state file that cannot be read is kept as `NAME.bad` and replaced by an empty one, with a `NOTE:` line on stderr. The text folder must not sit inside the library (that is a config error, exit 6).

Environment variables: `RESEARCHER_LIBRARY` (library for one run), `RESEARCHER_NO_POPUP=1` (never open a window, for scripts and tests), `RESEARCHER_HOST="name"` (the program name shown in the window title).

## 10. Command reference

```
researcher [SET ...]                    window, then print the paths of the ticked documents
researcher --setup SET                  window only: change the saved defaults, load nothing
researcher --no-popup [SET ...]         use the saved choices, ask nothing
researcher --list                       sets, sizes, saved defaults
researcher --sync                       convert the whole library to the text copy, load nothing
researcher --init [LIBRARY]             write a starter config
researcher --library PATH               use another library for this run
researcher --config PATH                use another config file
researcher --out DIR                    text folder for this run
researcher --sessions [--limit N]       list closed sessions (default 30)
researcher --rename ID NAME             rename a session
researcher --resume ID                  print the resume command
researcher --session-load ID            write one session as a .txt
researcher --context [PRESET ...]       window on earlier chats
researcher --start code|chats|research  the tab the window opens on
researcher --profile NAME [PRESET]      code + chats + research of a project
researcher --usage                      local usage totals
researcher --usage-export [FILE]        write the usage records to a file
researcher --usage-share on|off         share anonymous usage counts, or not
researcher --usage-send [--dry-run]     send the usage records not sent yet
researcher --version                    version
```

The original Python version (`skills/researcher/researcher.py`, Python 3.11 or newer, GTK 4 for its window) takes the same arguments, uses the same files and prints the same lines. The skill falls back to it when `researcher` is not on your PATH.

## 11. Output lines and exit codes

The program prints one line per event; the skill reads them for Claude.

| Line | Meaning |
|---|---|
| `<chars>  <path>.txt` | A loaded document. Claude reads it in full. |
| `DOC "name" (id)` | The display name of the next document (you renamed it). |
| `CHAT "name" (id)` | A loaded chat, followed by its path. |
| `CODE <path>` | A source file of a profile, followed by its size and full path. |
| `left out by choice (N)` | Documents you did not tick (named only when there are 20 or fewer). |
| `total: N docs, X chars (~T tokens)` | What was loaded. |
| `SAVED default for NAME: ...` | You kept your choice as the set's default. |
| `SAVED context preset ...` / `SAVED profile preset ...` | You saved a chat preset or a profile preset. |
| `RENAMED ...` | You renamed a chat or a preset in the window. |
| `LIBRARY SET: PATH` | First run: the folder you chose was saved. |
| `NO POPUP ...` | No display (or `--no-popup`): saved choices were used, or nothing was loaded if none exist. |
| `CANCELLED` | You closed the window. Nothing was loaded. |
| `TIMEOUT` | The window was open 540 s. Nothing was loaded. |
| `EMPTY LIBRARY` | The library folder holds no documents yet. |
| `OPENCODE SKIPPED (...)` | opencode's chats could not be read; Claude Code's chats still loaded. |
| `USAGE SHARING: not decided` | Sharing was never answered; the skill asks once. |
| `MISSING` / `UNREADABLE` | A source or a chat transcript is gone or cannot be read; the rest loaded. |

Exit codes:

| Code | Meaning |
|---:|---|
| 0 | ok |
| 1 | a usage send failed (the records stay queued) |
| 2 | unknown set |
| 3 | cancelled |
| 4 | the window timed out |
| 5 | custom set with no saved choice and no window |
| 6 | config or library problem |
| 7 | sessions are switched off |
| 8 | session id not found, or a prefix matches more than one |
| 9 | usage sharing is not switched on (`--usage-send`) |

## 12. Troubleshooting

| Problem | What to do |
|---|---|
| `NO LIBRARY` | Set `library` in `~/.config/researcher/config.toml`, run `researcher --init ~/research`, or set `RESEARCHER_LIBRARY`. |
| `CONFIG ERROR` | The message names the fix. The text folder inside the library is the usual cause. |
| `EMPTY LIBRARY` | Save or copy research (.html, .md, .txt, .pdf) into the library folder. |
| `NO POPUP` | There is no display. Run it in a desktop session, or use `--no-popup` on purpose. |
| `SESSIONS OFF` (exit 7) | Add `[sessions]` with `enabled = true` to the config. |
| `OPENCODE SKIPPED` | Set `opencode_cmd` under `[sessions]` to the full path of the `opencode` program. |
| A PDF shows no text | Install `poppler-utils` for `pdftotext`. A scanned PDF has no text to extract. |
| A document looks out of date | The text copy refreshes when the source changes. Force it by deleting that file in `~/.local/share/researcher/text/`, or run `--sync`. |
| A new paper is missing from a set | Add its id to that set's `docs` in the config (it shows as NEW until then). |
| A chat is missing from the list | It is the chat you are in (never listed), or sessions are off, or opencode chats are not switched on. |

Bugs and questions: [github.com/LudaLunizz/the-researcher/issues](https://github.com/LudaLunizz/the-researcher/issues). Security problems: see [SECURITY.md](../SECURITY.md).
