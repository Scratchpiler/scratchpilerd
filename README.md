# scratchpilerd

A language server for Scratchpiler (`.sdsl`), written in Rust. It is an exact port of Scratchpiler's
JS front end (lexer, parser, analyzer, lints, type checks and cross-sprite project analysis) plus the
Monaco editor providers (completion, hover, signature help). Its output is diffed against the JS byte
for byte.

## Running

```sh
scratchpilerd --project ~/games/platformer              # LSP over stdio
scratchpilerd --node-ipc | --pipe=<path> | --socket=<port> --project <dir>
scratchpilerd daemon --listen=/tmp/sp.sock --project <dir> [--idle-exit 600]
scratchpilerd check --project .                         # print problems, exit 1 on errors
scratchpilerd warm  --project .                         # fill the disk cache
```

The project folder is never guessed. Pass `--project`, or send `initializationOptions.project`.
A project folder contains:

- `project.json`, or exactly one `.sb3`: the index of sprites, variables, lists, costumes and sounds.
  You can also push one with the `scratchpiler/setIndex` notification.
- `Stage.sdsl` plus one `<Sprite>.sdsl` per sprite. Characters that are invalid in file names are
  written as `%XX`.
- Headers: `*.h` files in the root, `include/` or `headers/`.
- Optionally, a `scratchpiler.json` (`project`, `stage`, `sprites`, `headers`) to override the layout.

## Features

- Diagnostics for every file, including cross-sprite checks.
- hover, completion, signatureHelp, definition, references, documentHighlight, rename across sprites,
  semanticTokens (full and range), inlayHint (broadcast listener counts), documentSymbol,
  workspace/symbol, formatting, foldingRange.
- `$/progress` reports.
- Custom methods: `scratchpiler/eventGraph`, `scratchpiler/projectProblems`, `scratchpiler/status`.

Settings arrive in `initializationOptions.settings`, or through `workspace/didChangeConfiguration`
under `scratchpiler`:

- Lint toggles: `lintUnreachable`, `lintOrphaned`, `lintTypecheck`, `lintSemantic`, `lintSmells`,
  `lintProject`.
- `inlayHints`.
- `semanticTokens`: `full` or `semantic`.
- `completionFollowUps`.
- `completionRanking` (default on): see [Completion ranking](#completion-ranking).
- `cache`.

## Completion ranking

The candidate list is the Monaco provider's, diffed against the JS. On top of it, the server sets
`sortText` from where the cursor is, so the useful entries come first:

- **Top level**: `on …`, `define`, `scratchroutine`, `struct`, `enum` and `#include` first; plain
  statements are orphans and sink. `#include` leads while the file has no code yet. In header files hats
  sink and declarations lead.
- **Statement position** (inside a block, at the start of a statement): commands and control flow
  first. After a block that closes an `if`, `else {}` and `else if {}` lead. `break`/`continue` lead in
  loops, `case`/`default` directly in a `match`, `return value` in a `returns` block, `checkCancel()` in a
  scratchroutine.
- **Expression position** (after `set … to`, `if`, an operator, or inside call parentheses): parameters and
  loop variables, then variables the file uses, then enum constants and values. The entries that fit the
  slot rise: comparisons, `touching`, `key` and `not` after `if`/`while`/`until`; numeric reporters and
  math inside `move(`, `setX(`, `repeat`; text reporters inside `say(`; list variables for a list
  parameter such as the second argument of `listAdd`.
- **Usage**: within a tier, commands, custom blocks and variables the current file uses more often rank
  higher.

Nothing is removed, only reordered, so a wrong guess costs a keystroke and not a missing entry. Set
`completionRanking` to `false` for the unranked list. `scratchpilerd debug editor` and `tools/diff-editor.mjs`
always use the unranked list, so the JS parity stays exact.

## Threads and priorities

Work runs in three lanes, so completion, hover and navigation never wait behind a whole-project pass:

- **interactive**: 2–4 threads at normal priority. Runs requests against an immutable snapshot.
- **edits**: one thread with a latest-wins mailbox. Recomputes diagnostics for the file being edited.
- **background**: cores − 1 threads at lowered OS priority (nice 10, QoS utility, or
  BELOW_NORMAL). Analyzes the whole project, links it and writes the cache.

Each document version is an `Arc` snapshot whose analysis is computed at most once, by whichever lane
asks first. When nothing is happening, every thread is blocked, so an idle server uses no CPU.

## Disk cache

Each project gets a bincode file in `$XDG_CACHE_HOME/scratchpilerd`, `~/Library/Caches/scratchpilerd`
or `%LOCALAPPDATA%\scratchpilerd\cache`. Override the location with `--cache-dir` or
`SCRATCHPILERD_CACHE_DIR`, or disable it with `--no-cache`.

- **Entries**: keyed by an xxh3 hash of the text, sprite, index, included headers and server version.
- **Index**: the parsed project index, keyed by the index file's size and mtime.

## Testing

```sh
cargo test --release                     # unit tests + every transport, daemon, check/warm, cache
node tools/corpus.mjs <sb3s...>          # decompile real projects into target/corpus
node tools/diff.mjs ast 400              # parser vs JS
node tools/diff.mjs diag 400             # tokens, symbols, occurrences, diagnostics vs JS
node tools/diff-project.mjs 400          # cross-sprite analysis vs JS
node tools/diff-editor.mjs 60            # completion / hover / signature help vs language.js
node tools/bench.mjs <project dirs...>   # time, CPU, memory, idle wakeups, request latency
```

The `tools/` scripts expect a `../scratchpiler` checkout with esbuild installed. Set
`SCRATCHPILER_DIR` to use a different checkout. `node tools/extract.mjs` regenerates
`src/data/generated.rs` from language.js.

CI (`.github/workflows/ci.yml`) runs fmt and clippy, then tests and builds for x86_64 Linux, Windows
x86_64 and ARM64, and macOS ARM64 and Intel. After fmt, clippy and all platform builds pass, pushes
to `main` (or manual CI runs on `main`) publish `v<package.version>` from `Cargo.toml` with the release
archives and SHA-256 checksums that the Zed extension downloads. Published versions are skipped;
bump the package version to release an update. Tagging `v*` also publishes that tag, with tags
containing `-` marked as prereleases. The Zed extension downloads the latest stable release.
The JS parity job runs on demand.
