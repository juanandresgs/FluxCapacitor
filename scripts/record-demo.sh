#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "native demo recording requires macOS" >&2
  exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
ghostty_app="/Applications/Ghostty.app"
ghostty_bin="$ghostty_app/Contents/MacOS/ghostty"
session="$root/scripts/demo-session.sh"
config="$(mktemp -t flux-ghostty.XXXXXX)"
recording="$(mktemp -t flux-demo.XXXXXX).mov"
frame="$(mktemp -t flux-demo-frame.XXXXXX).png"
before="$(mktemp -t flux-ghostty-pids.XXXXXX)"
window_pid=""

cleanup() {
  [[ -n "$window_pid" ]] && kill "$window_pid" 2>/dev/null || true
  "$session" cleanup 2>/dev/null || true
  rm -f "$config" "$recording" "$frame" "$before"
}
trap cleanup EXIT

for command in ffmpeg ffprobe osascript screencapture; do
  command -v "$command" >/dev/null || {
    echo "missing required command: $command" >&2
    exit 1
  }
done
[[ -x "$ghostty_bin" ]] || {
  echo "Ghostty is required at $ghostty_app" >&2
  exit 1
}

ghostty_pids() {
  pgrep -f "^$ghostty_bin( |$)" 2>/dev/null | sort -n || true
}

ghostty_pids > "$before"

cat > "$config" <<EOF
font-family = SF Mono
font-size = 16
font-thicken = false
adjust-cell-height = 5%
background = #121215
foreground = #d8d8dc
palette = 0=#121215
palette = 1=#e07474
palette = 2=#74d69a
palette = 3=#e2b468
palette = 4=#75a4e8
palette = 5=#c48fff
palette = 6=#68beff
palette = 7=#eeeeef
palette = 8=#707078
palette = 9=#ff9191
palette = 10=#93edaf
palette = 11=#f4ca85
palette = 12=#91b8f4
palette = 13=#dca3ff
palette = 14=#8ad2ff
palette = 15=#ffffff
window-width = 120
window-height = 38
window-position-x = 248
window-position-y = 80
window-padding-x = 8
window-padding-y = 8
window-padding-balance = true
window-save-state = never
macos-titlebar-style = hidden
shell-integration = none
mouse-hide-while-typing = true
initial-command = $session run
EOF

open -na Ghostty.app --args --config-file="$config"

for _ in {1..50}; do
  while read -r candidate; do
    [[ -z "$candidate" ]] && continue
    if ! grep -qx "$candidate" "$before"; then
      window_pid="$candidate"
      break 2
    fi
  done < <(ghostty_pids)
  sleep 0.1
done

[[ -n "$window_pid" ]] || {
  echo "could not identify the isolated Ghostty process" >&2
  exit 1
}

window_rect=""
for _ in {1..80}; do
  window_rect="$(osascript - "$window_pid" <<'APPLESCRIPT' 2>/dev/null || true
on run argv
  set targetPid to item 1 of argv as integer
  tell application "System Events"
    tell first application process whose unix id is targetPid
      if (count of windows) is 0 then return ""
      set windowPosition to position of first window
      set windowSize to size of first window
      return (item 1 of windowPosition as text) & "," & (item 2 of windowPosition as text) & "," & (item 1 of windowSize as text) & "," & (item 2 of windowSize as text)
    end tell
  end tell
end run
APPLESCRIPT
)"
  [[ -n "$window_rect" ]] && break
  sleep 0.1
done

[[ -n "$window_rect" ]] || {
  echo "could not determine the Ghostty window bounds" >&2
  exit 1
}

sleep 0.35
screencapture -x -v -V12.8 -R"$window_rect" "$recording"

ffmpeg -hide_banner -loglevel error -y -i "$recording" \
  -vf "fps=30,scale=1520:-2:flags=lanczos,format=yuv420p" \
  -c:v libx264 -preset slow -crf 20 -movflags +faststart \
  "$root/assets/demo.mp4"

ffmpeg -hide_banner -loglevel error -y -i "$recording" \
  -vf "fps=15,scale=1200:-2:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=192:stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle" \
  -loop 0 "$root/assets/demo.gif"

ffmpeg -hide_banner -loglevel error -y -ss 8 -i "$root/assets/demo.mp4" -frames:v 1 "$frame"
echo "recorded native Ghostty demo"
echo "preview: $frame"
ffprobe -v error -show_entries format=duration,size -of default=noprint_wrappers=1 "$root/assets/demo.mp4"
