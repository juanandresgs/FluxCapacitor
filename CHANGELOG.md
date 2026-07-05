# Changelog

All notable changes to Flux Capacitor are documented here.

## Unreleased

- Add an event-triggered `WORK` timeline for authoritative Beads issue transitions.
- Suppress raw Beads/Dolt storage churn while preserving visible configuration edits.
- Validate WORK delivery against Flux's own live Beads project history.

## 0.1.0 - 2026-07-04

- Add a live terminal timeline for native filesystem and Git events.
- Show contextual file diffs and semantic Git operations.
- Support multiple watched roots with stable workspace labels and focus controls.
- Discover and follow linked Git worktrees automatically.
- Surface watcher integrity failures without inferring agent identity or intent.
