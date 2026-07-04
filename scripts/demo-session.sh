#!/usr/bin/env bash
set -euo pipefail

base="/tmp/flux-capacitor-demo"
main="$base/main"
linked="$base/agent-worktree"

cleanup() {
  rm -rf "$base"
}
trap cleanup EXIT
cleanup
mkdir -p "$main/src"

git -C "$main" init -q
git -C "$main" config user.name "Flux Demo"
git -C "$main" config user.email "flux@example.invalid"
printf 'pub fn status() -> &\x27static str { "starting" }\n' > "$main/src/status.rs"
git -C "$main" add src/status.rs
git -C "$main" commit -qm "initial state"

(
  sleep 2
  git -C "$main" worktree add -q -b agent/live-demo "$linked"
  sleep 2
  cat > "$main/src/status.rs" <<'CHANGE'
pub fn status() -> &'static str {
    "main workspace updated"
}
CHANGE
  sleep 1.5
  cat > "$linked/agent-notes.md" <<'CHANGE'
# Agent worktree

- reviewing event flow
CHANGE
  sleep 1.5
  cat >> "$linked/agent-notes.md" <<'CHANGE'
- linked folder is visible
CHANGE
  sleep 1.5
  mv "$linked/agent-notes.md" "$linked/implementation-notes.md"
  sleep 1.5
  git -C "$linked" add implementation-notes.md
  git -C "$linked" commit -qm "record worktree progress"
) >/dev/null 2>&1 &
producer=$!

flux "$main"
wait "$producer" 2>/dev/null || true
