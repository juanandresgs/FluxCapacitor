# Flux Capacitor

`flux` is a native terminal observer for work performed inside one or more directory trees. It reports only changes supported by direct filesystem or Git metadata events—without wrapping tools, polling repository state, or guessing which process caused an action.

## Build and run

```sh
cargo build --release
./target/release/flux /path/to/workspace /path/to/another-workspace
```

Install it on your `PATH` with:

```sh
cargo install --path .
flux /path/to/workspace
```

With no paths, `flux` watches the current directory.

## Event families

### FILE

Native events from watched directory trees:

- File and directory creation, modification, deletion, movement, and rename
- Contextual text diffs for readable files up to 1 MiB
- Metadata-only events for binary, unreadable, and oversized files
- Paired and split rename handling using native tracker IDs where available
- Short event-burst coalescing that updates the visible event to the latest observed state

Flux reports the filesystem fact only. It does not identify the process, agent, command, or intent behind a change.

### GIT

Native events from each repository’s Git metadata directories trigger a single state comparison after a 120 ms quiet window. There is no periodic repository polling.

Currently classified:

- Commits and merge commits
- Branch checkout and detached-HEAD transitions
- Forward commits, backward resets, and divergent history rewrites
- Local branch creation, update, and deletion
- Tag creation, update, and deletion
- Remote-tracking reference creation, update, and deletion
- Stash creation, update, and deletion
- Merge, rebase, cherry-pick, revert, and bisect state starting or ending
- Repositories initialized while Flux is already watching
- Linked worktree metadata and shared Git references

When Git operation state disappears, Flux says the operation **ended**. It does not guess whether the operation completed or was aborted unless the resulting repository state proves a more specific transition.

### INTEGRITY

Events describing whether observation itself remains trustworthy:

- Filesystem and Git metadata watches established
- Native watcher errors and watch-limit failures
- Explicit FSEvents/inotify rescan sentinels indicating dropped events
- Filesystem or Git event-channel disconnection
- Watched-root removal
- Changed paths that could not be inspected

Integrity levels are `INFO`, `DEGRADED`, `UNCERTAIN`, and `LOST`. An uncertainty or loss remains visible in the global status even if the event timeline is cleared. Flux does not perform a reconciliation scan, so it never claims to reconstruct an event gap.

## Keys

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Move through the timeline |
| `/` | Filter by path or summary |
| `1`–`4` | Toggle FILE modify/create/move/delete actions |
| `5` | Toggle GIT events |
| `6` | Toggle INTEGRITY events |
| `space` | Pause or resume the stream |
| `c` | Clear retained timeline events |
| `g` / `G` | Jump to newest/oldest |
| `q`, `Ctrl-C` | Quit |

## Operational behavior

- Seeds an initial filesystem snapshot silently, so existing files do not flood the timeline.
- Uses native recursive filesystem notifications through `notify`.
- Ignores `.git`, `node_modules`, `target`, `dist`, and `coverage` in the FILE stream.
- Observes `.git` separately through the semantic GIT pipeline.
- Retains 1,000 events in memory by default; adjust with `--max-events`.
- Emits no desktop notifications and sends no data elsewhere.
- Restores the terminal after normal exit or panic.

See `docs/IMPLEMENTATION_STATUS.md` for the evidence-backed quality and completeness assessment.
