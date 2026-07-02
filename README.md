# Flux Capacitor

`flux` is a native terminal application for passively observing filesystem activity from coding agents and other tools. It runs as one Rust binary, uses the operating system's filesystem events, and keeps a navigable timeline with contextual diffs.

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

With no paths, `flux` watches the current directory:

```sh
cargo run --release
```

## Keys

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Move through the timeline |
| `/` | Filter by path |
| `1`–`5` | Toggle modify/create/move/delete/Git events |
| `space` | Pause or resume the stream |
| `c` | Clear the timeline |
| `g` / `G` | Jump to newest/oldest |
| `q`, `Ctrl-C` | Quit |

## Behavior

- Seeds an initial snapshot silently, so existing files do not flood the timeline.
- Watches several directory trees recursively using native OS events.
- Shows line-level text diffs for files up to 1 MiB and metadata for larger or binary files.
- Handles paired and split rename events and buffers incomplete rename events briefly.
- Subscribes to native Git metadata events and emits commits, merge commits, checkouts, resets, history rewrites, and local branch creation/deletion without exposing raw `.git` noise.
- Ignores `.git`, `node_modules`, `target`, `dist`, and `coverage` trees.
- Retains 1,000 events by default; adjust with `--max-events`.
- Stays local, emits no notifications, and restores the terminal after errors or panics.
