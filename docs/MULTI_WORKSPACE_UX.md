# Multi-Workspace UX

Assessment date: July 2, 2026

## Chosen model

Flux keeps one chronological, observed-order event stream. Each event receives a stable workspace identity derived from the configured root, shown as the same colored label in the timeline and preview. This preserves the primary question—“what changed most recently?”—while making source attribution visible without requiring another scan across the row.

Flux does not create agent lanes or infer agent identity. Native filesystem and Git metadata events prove where a change was observed, but not which process or agent caused it.

## Implemented behavior

- Shortest unique suffix labels keep common roots compact: two roots named `api` become labels such as `team/api` and `other/api`.
- Workspace colors are stable for the process lifetime and assigned by command-line root order.
- `w` and `W` cycle focus through every workspace and `ALL`; filtering does not reorder or discard retained events.
- Overlapping roots use deepest-root ownership for path attribution.
- The event loop handles at most 512 filesystem messages per render pass and displays `CATCHING UP` while yielding, preventing a busy producer from starving keyboard input and rendering.
- Selection remains event-based within the visible filtered list, and the workspace label is repeated in the preview for immediate correlation.

## Why this follows proven patterns

- Kubernetes log tooling can prefix each line with its pod/container source when combining streams. Flux applies the same source-prefix principle using the directly known workspace root: <https://kubernetes.io/docs/reference/kubectl/generated/kubectl_logs/>
- Grafana live logs use contrasting treatment for newly arrived entries and provide pause/resume controls. Flux already uses temporary newness highlighting and pause buffering, which remain useful when several roots are active: <https://grafana.com/docs/grafana/latest/visualizations/explore/logs-integration/>
- Grafana Loki recommends a small set of stable, low-cardinality labels aligned with actual query dimensions. Configured workspace roots satisfy that rule; paths and supposed agent identities do not: <https://grafana.com/docs/loki/latest/get-started/labels/>

## Scenarios

### Independent agents in independent roots

Events interleave by native callback arrival. Workspace labels make the interleaving legible, while workspace focus provides a quick isolation mechanism. Flux should not imply causal order when timestamps are effectively simultaneous.

### Several agents in one root

The workspace remains the only certain source dimension. File paths, event kinds, Git transitions, and native rename trackers remain valid; agent identity and task grouping remain unavailable without wrapping or external instrumentation.

### Bursty generated activity

Existing ignore rules suppress known high-volume trees. Remaining events are processed in bounded render batches. Burst coalescing stays path-local so unrelated simultaneous changes are never merged into a fictional task.

### Same-named or overlapping roots

Same-named roots gain unique suffix labels. For overlapping roots, the deepest configured root owns descendant events. The parent root still receives events outside that child tree.

## Recommended next steps

1. Add a workspace picker overlay once cycling becomes cumbersome, likely beyond roughly eight roots. Keep `w` as the fast path.
2. Add per-workspace observer-health indicators. Global health alone can hide which root lost observation.
3. Show exact native queue depth beside `CATCHING UP` if sustained high-volume workloads make backlog diagnosis important.
4. Add optional per-workspace activity counts over a short rolling window, but avoid animation that competes with the event stream.
5. Add rendering snapshots for narrow terminals and large workspace counts.
6. Add Linux and Windows multi-root runtime tests because native watcher event shapes and resource limits differ by platform.

A permanent swimlane-per-workspace view is not recommended as the default. It weakens global recency, consumes terminal width, and becomes unstable as root count grows. It may be useful later as an explicit secondary mode for comparing a small number of roots.
