#!/usr/bin/env bash
set -euo pipefail

base="/tmp/flux-demo"
main="$base/storefront"
linked="$base/agent-checkout"
pid_file="$base/producer.pid"

cleanup() {
  if [[ -f "$pid_file" ]]; then
    kill "$(cat "$pid_file")" 2>/dev/null || true
  fi
  rm -rf "$base"
}

prepare() {
  cleanup
  mkdir -p "$main/src"

  git -C "$main" init -q
  git -C "$main" config user.name "Flux Demo"
  git -C "$main" config user.email "flux@example.invalid"
  cat > "$main/src/checkout.rs" <<'SOURCE'
pub fn total(n: u64) -> u64 { n }
SOURCE
  git -C "$main" add src/checkout.rs
  git -C "$main" commit -qm "initial storefront"

  nohup "$0" produce >/dev/null 2>&1 &
  echo $! > "$pid_file"
}

produce() {
  sleep 3
  cat > "$main/src/checkout.rs" <<'SOURCE'
pub fn total(n: u64) -> u64 { n + n / 20 }
SOURCE

  sleep 2.5
  git -C "$main" worktree add -q -b agent/express-checkout "$linked"

  sleep 2.5
  cat > "$linked/src/express.rs" <<'SOURCE'
pub fn express_checkout() -> bool { true }
SOURCE

  sleep 2.5
  git -C "$linked" add src/express.rs
  git -C "$linked" commit -qm "add express checkout"
}

case "${1:-}" in
  prepare) prepare ;;
  produce) produce ;;
  cleanup) cleanup ;;
  *)
    echo "usage: $0 {prepare|cleanup}" >&2
    exit 2
    ;;
esac
