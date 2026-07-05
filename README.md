# Flux Capacitor

**Watch coding agents change your projects in real time.**

A native terminal timeline for filesystem and Git activity. No agent wrappers, polling, or vendor integration.

![Flux following a newly created Git worktree and showing live file and Git events](https://raw.githubusercontent.com/juanandresgs/FluxCapacitor/main/assets/demo.gif)

## Install

Flux has not published its first binary release yet. Install the current alpha from source:

```sh
cargo install --git https://github.com/juanandresgs/FluxCapacitor --locked
```

Binary installers and Homebrew instructions will be added only after their release artifacts are published and verified.

## Run

```sh
flux ~/Code/project
flux ~/Code/api ~/Code/web ~/Code/worker
```

## What You See

- File creation, modification, movement, deletion, and contextual diffs.
- Commits, merges, checkouts, branches, tags, stashes, and rewrites.
- Stable workspace labels across multiple projects and concurrent agents.
- Linked Git worktrees joining the timeline automatically.
- Watcher failures and dropped-event signals reported explicitly.

Flux consumes native filesystem notifications. It does not wrap agents, poll repositories, or guess which process caused a change.

Memory is bounded: callback queues report overflow as `INTEGRITY`, paused history obeys `--max-events`, and baselines default to 200,000 paths plus 64 MiB of text configurable with `--max-snapshots` and `--max-snapshot-bytes`.

## Controls

| Key | Action |
| --- | --- |
| `/` | Filter events |
| `w` / `W` | Cycle workspace focus |
| `t` | Toggle tracked folders |
| `space` | Pause or resume |
| `q` | Quit |

## Status

Flux is an alpha. macOS is the primary interactive platform. Linux and Windows run native filesystem/Git smoke tests in CI but still need broader field use.

[Implementation details](docs/IMPLEMENTATION_STATUS.md) · [Production readiness](docs/PRODUCTION_READINESS.md) · [Download the demo MP4](assets/demo.mp4)

MIT licensed.
