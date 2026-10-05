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
- `cache`.

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
x86_64 and ARM64, and macOS ARM64 and Intel. Tagging `v*` publishes the release archives that the Zed
extension downloads. The JS parity job runs on demand.
