# Flux Capacitor

`flux` is a fast terminal timeline for live filesystem and Git activity across one or more directory trees. It uses native filesystem notifications, does not wrap agent tools, and does not poll for changes.

> **Status:** useful alpha. Reliable for local observation on macOS; Linux and Windows support is built on cross-platform libraries and exercised in CI, but needs more real-world use before a stable claim.

## Install

Requires Rust 1.88 or newer. Git is optional, but required for semantic Git events.

```sh
cargo install --git https://github.com/juanandresgs/FluxCapacitor
```

The repository must be public before other people can use that command. Until then, clone it and run:

```sh
git clone https://github.com/juanandresgs/FluxCapacitor
cd FluxCapacitor
cargo install --path .
```

## Use

```sh
flux                         # watch the current directory
flux ~/code/api ~/code/web   # watch multiple directory trees
flux --max-events 2000 .     # change in-memory retention
```

Flux shows one observed-order timeline. With multiple roots, every event carries a stable colored workspace label; `w` and `W` cycle workspace focus.

When any watched root is a Git repository, Flux automatically includes its linked worktrees at startup. New worktrees join after native Git metadata reports their creation, regardless of which agent or tool created them. Press `t` to show or hide the tracked-folder drawer at the bottom.

### Event families

- **FILE:** create, modify, rename/move, and delete events, with contextual diffs for readable files up to 1 MiB.
- **GIT:** commits, merges, checkouts, resets/rewrites, branches, tags, remotes, stashes, and operation-state transitions triggered by native `.git` events.
- **INTEGRITY:** watcher establishment, read failures, dropped-event signals, channel failures, watch exhaustion, and root removal.

Flux reports observable facts. It does not infer the responsible process, agent, command, task, or intent.

## Keys

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Move through events |
| `g` / `G` | Jump to newest/oldest |
| `/` | Filter by path or summary |
| `w` / `W` | Cycle workspace focus |
| `t` | Show/hide tracked folders |
| `1`–`6` | Toggle event categories |
| `space` | Pause/resume the stream |
| `c` | Clear retained events |
| `q`, `Ctrl-C` | Quit |

## Guarantees and limits

- Native recursive notifications feed the timeline; Git state is read only after matching metadata activity.
- Existing files seed the initial snapshot silently.
- Timeline history is memory-only and bounded to 1,000 events by default.
- Native watcher backends may coalesce writes. Cross-root order is callback arrival order, not proof of causality.
- Flux reports explicit dropped-event/rescan signals but does not reconstruct missing history.
- Linked Git worktrees are discovered automatically. Unrelated clones and arbitrary external directories still need to be supplied explicitly.
- Generated trees such as `.git`, `node_modules`, `target`, `dist`, and `coverage` are excluded from FILE events.

See [`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md) for the detailed evidence and limitations, and [`docs/PRODUCTION_READINESS.md`](docs/PRODUCTION_READINESS.md) for release readiness.

## Develop

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo run -- .
```

Licensed under the [MIT License](LICENSE).
