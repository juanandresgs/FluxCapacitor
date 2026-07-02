# Implementation Status

Assessment date: July 2, 2026

## Executive assessment

Flux is a functional alpha with a sound event-driven architecture. The core FILE timeline is strong for ordinary coding activity, the GIT stream covers the most useful directly observable repository transitions, and INTEGRITY now reports explicit loss-of-confidence signals rather than hiding them.

It is not yet appropriate to describe the timeline as a complete forensic record. Native watcher backends may coalesce activity, platform behavior differs, history is memory-only, and Flux deliberately does not reconcile gaps after the operating system reports dropped events.

| Area | Assessment | Summary |
| --- | --- | --- |
| Architecture | Strong | Native event streams, no repository polling, bounded in-memory state, clean TUI separation |
| FILE | Strong alpha | Main operations and diffs work; boundary moves and complex directory renames need more platform testing |
| GIT | Good alpha | Broad semantic ref/state coverage; some repository layouts and ambiguous operation outcomes remain intentionally limited |
| INTEGRITY | Good foundation | Explicit rescan, error, channel, root-loss, and read-failure reporting; no automatic recovery or reconciliation |
| Tests | Moderate | Sixteen deterministic tests including real native Git commit/ref events and macOS-imprecise rename shapes; platform matrix remains narrow |
| Portability | Unproven beyond macOS | Built on cross-platform crates, but runtime behavior has only been exercised locally on macOS |
| Persistence | Not implemented | Timeline is intentionally process-local and memory-only |

## Interface hierarchy

- Timeline rows use a two-line layout: event family/action and path first, then event ID, context, diff counts, freshness, and age.
- The selected row and preview share the same event-colored badge, event ID, and border color, creating a direct visual link between panes.
- New events receive a short-lived marker, background, and `NEW` label that fades after three seconds without requiring notifications.
- FILE diff totals use filled add/remove chips, while changed lines retain add/remove coloring in the preview.
- INTEGRITY severity remains separate from the event-family badge so the category and observer state are both immediately visible.
- The same hierarchy is used in stacked 80-column and split-pane wide layouts.

## FILE implementation

### Implemented

- Recursive native watching for multiple roots.
- Silent initial snapshots.
- File and directory create, modify, remove, and rename events.
- Paired rename events and split rename events.
- Native rename tracker IDs are used to avoid mispairing concurrent split renames.
- One-path rename events, as emitted by imprecise FSEvents mode, are paired using snapshot existence and pending rename state.
- Unpaired rename-from events become deletes after a 400 ms window.
- Unpaired rename-to events become creates.
- Text snapshots and contextual line diffs up to 1 MiB.
- Binary and oversized-file metadata events.
- A 300-line diff preview limit.
- Eighty-millisecond burst coalescing for duplicate create/modify and rename/modify sequences.
- Coalesced events are updated to the latest observed file size and diff instead of silently discarding the later state.
- A create signal for an already snapshotted path is treated as replacement/modification rather than a new file.
- Search, action filters, pause buffering, and bounded retention.

### Known limitations

- A move from a watched root to an unwatched location is generally observable as a delete; the destination is outside Flux’s evidence boundary.
- A move from an unwatched location into a watched root is generally observable as a create.
- Directory renames depend on backend events for descendant paths. Flux does not currently remap every cached descendant snapshot as one atomic subtree operation.
- Native backends may coalesce several physical writes into one event. Flux reports the state observed when it handles the event, not every write syscall.
- Invalid UTF-8 is treated as non-text and receives no textual diff.
- Ignore rules are currently compiled into the application rather than configurable.
- The timeline is not persisted across runs.

### Quality judgment

The FILE implementation is suitable for live observation of normal agent editing. It is not a filesystem journal and should not be represented as one.

## GIT implementation

### Event source

Flux discovers the repository and actual Git directory with Git itself, then subscribes to native events for that metadata location. For linked worktrees, it also watches the shared common Git directory. Incoming metadata activity starts a 120 ms quiet window; only then does Flux read repository state once and compare it with the previous snapshot.

This is event-triggered debouncing, not periodic state polling.

### Implemented classifications

- Commit when HEAD advances on the current branch.
- Merge commit when the new commit has multiple parents.
- Checkout when the symbolic branch changes.
- Entry into or exit from detached HEAD.
- Reset when HEAD moves to an ancestor.
- Rewrite when HEAD changes without either commit being an ancestor of the other.
- Local branch creation, update, and deletion.
- Tag creation, movement, and deletion.
- Remote-tracking reference creation, update, and deletion.
- Stash creation, update, and deletion through `refs/stash`.
- Merge, rebase, cherry-pick, revert, and bisect state start/end.
- Repository initialization detected from native `.git` creation events.
- Git metadata watcher errors and rescan sentinels forwarded to INTEGRITY.

### Deliberate certainty rules

- Operation-state disappearance is reported as “ended,” not “completed” or “aborted.”
- A backward HEAD movement is reported as a reset-state transition; Flux does not claim a particular command invocation.
- Remote-tracking updates report reference movement, not that a fetch or pull command ran.
- Stash events report `refs/stash` movement, not the exact user command.

### Known limitations

- Git must be installed and executable because event-triggered classification uses read-only Git commands.
- Pre-existing nested repositories are not recursively discovered at startup unless explicitly supplied as watched roots. Repositories initialized after startup are detected.
- Bare repositories are not currently supported because discovery relies on `--show-toplevel`.
- Reflog-only activity that leaves all tracked state unchanged produces no semantic event.
- Git notes, replace refs, bisect progress steps, and arbitrary custom refs are not classified.
- Operation-state files are Git implementation details; unusual Git versions or third-party implementations may differ.
- A rapid sequence may be intentionally collapsed into one final state comparison by the quiet window.

### Quality judgment

The GIT implementation is broad enough to be useful and remains faithful to the event-driven constraint. Its strongest area is reference and HEAD movement. Complex porcelain workflows are reported conservatively at the state-transition level.

## INTEGRITY implementation

### Implemented

- INFO events when FILE and GIT watches are established.
- DEGRADED events when an affected path cannot be inspected.
- UNCERTAIN events for native `Rescan` sentinels emitted by macOS FSEvents and Linux inotify when events may have been dropped.
- UNCERTAIN classification for watcher errors explicitly describing overflow, dropped events, or a required rescan.
- LOST events for native watch exhaustion, missing watch targets, event-channel disconnection, and watched-root removal.
- Global observer state retains the highest observed severity even when timeline events are cleared.
- Git watcher errors use the same integrity model as filesystem watcher errors.

### Known limitations

- Flux does not automatically rebuild snapshots or reconcile state after an UNCERTAIN event. That is intentional: a scan could show current state but could not reconstruct the missing event timeline.
- Flux does not currently attempt to re-establish a lost watch at runtime.
- Integrity is summarized globally in the header rather than tracked independently per root in the status area. Individual events retain their root.
- Some platform backends may report generic errors without enough information to distinguish DEGRADED from LOST conclusively; Flux uses the most conservative classification supported by the error.
- If the operating system fails silently and emits neither an error nor a rescan sentinel, Flux cannot detect that condition.

### Quality judgment

INTEGRITY is a solid honesty layer, not a recovery system. It prevents known gaps from being silently presented as complete observation.

## Test evidence

The current automated suite covers:

1. Text diff construction.
2. Ignore-rule boundaries.
3. Overflow-error integrity classification.
4. FILE create/modify/rename/delete translation.
5. Burst coalescing preserving the latest visible state.
6. Concurrent split rename pairing by native tracker ID.
7. Watched-root removal producing LOST integrity.
8. Native rescan sentinel producing UNCERTAIN integrity.
9. Timeline clearing preserving observer health.
10. Git object-ID formatting.
11. Git metadata path to worktree mapping.
12. Tag, remote-ref, stash, and operation-state classification.
13. One-path rename events paired from snapshot evidence.
14. Replacement create signals classified as modifications.
15. A real repository commit, tag, and branch creation waking the native Git watcher.
16. Repository initialization joining the stream from a `.git` creation event.

## Recommended next hardening work

1. Add Linux and Windows CI/runtime tests for watcher event shapes.
2. Add linked-worktree integration tests rather than relying only on normal-repository native tests.
3. Remap descendant snapshots atomically for directory-tree renames.
4. Add per-root integrity state and explicit watch-recovery support without claiming gap reconstruction.
5. Add configurable ignore patterns and retention settings.
6. Add TUI rendering snapshots at narrow and wide terminal sizes.

Categories intentionally excluded under the current evidence standard: PROCESS, MODE, DEPS, BUILD, TEST, AGENT, and inferred task grouping.
