# Changelog

All notable changes to Flux Capacitor are documented here.

## Unreleased

- Bound filesystem and Git callback queues and report dropped events as uncertain integrity.
- Bound paused history and add a configurable text-snapshot memory budget.
- Preserve descendant diff baselines across directory renames.
- Keep timeline selection stable when hidden filtered events arrive.
- Apply ignore rules relative to watched roots so legitimate roots named `target` or `dist` remain visible.
- Track integrity per root and source and retry recoverable filesystem and Git metadata watches.
- Restore terminal state through partial startup failures and termination signals.
- Keep demo media out of the crates.io package and advertise only available installation methods.
- Add explicit native runtime smoke jobs for macOS, Linux, and Windows.

## 0.1.0 - 2026-07-04

- Add a live terminal timeline for native filesystem and Git events.
- Show contextual file diffs and semantic Git operations.
- Support multiple watched roots with stable workspace labels and focus controls.
- Discover and follow linked Git worktrees automatically.
- Surface watcher integrity failures without inferring agent identity or intent.
