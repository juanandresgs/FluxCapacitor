# Production Readiness

Assessment date: July 4, 2026

## Verdict

Flux is ready to share publicly as an **alpha developer tool**, not as a production-grade audit trail or stable cross-platform utility.

The core architecture is appropriate for the product: native event streams, conservative classification, bounded queues and retained history, explicit integrity events, per-root recovery, and no unsupported agent/process attribution. Local macOS behavior is strong and native smoke cases run across the three CI platforms. The main remaining readiness gaps are interactive platform evidence and release verification—not the basic interaction model.

## Evidence

- `cargo test --locked`: 35 tests pass locally, including runtime linked-worktree discovery, directory-tree baseline remapping, bounded retention, watch recovery, and real multi-root filesystem and Git activity.
- `cargo clippy --locked --all-targets -- -D warnings`: passes.
- `cargo fmt --check`: passes.
- `cargo audit`: no known RustSec advisories in the locked dependency graph.
- `cargo publish --dry-run`: packages and rebuilds successfully.
- Package media is excluded by an explicit Cargo include list; CI rejects source packages larger than 256 KiB.
- GitHub Actions runs the full Rust 1.88 suite plus explicit native filesystem, recovery, Git, and linked-worktree smoke cases on current Ubuntu, macOS, and Windows runners.
- Local runtime testing: macOS on Apple Silicon.
- Repository state at assessment: public, with tag-triggered cross-platform release automation configured; no release tag has been published yet.

## Release gates

### Alpha platform matrix

| Platform | Automated evidence | Current claim |
| --- | --- | --- |
| macOS (`macos-latest`) | Full tests plus native filesystem, recovery, Git, and linked-worktree smoke cases | Primary interactive platform |
| Linux (`ubuntu-latest`) | Full tests plus the same native smoke cases | Alpha CI support; more field use needed |
| Windows (`windows-latest`) | Full tests plus the same native smoke cases | Alpha CI support; more field use needed |

Flux does not yet promise minimum OS versions. The first tagged release should replace rolling runner labels with an explicit tested version floor.

### Required before advertising binary installation

1. **Tag the first release.** Use `v0.1.0` and keep the project explicitly labeled alpha.
2. **Verify the release artifacts.** Install the generated macOS, Linux, and Windows archives on their native platforms and verify the shell, PowerShell, and Homebrew paths.

The public repository, MIT license, Cargo metadata, changelog, release workflow, installer generation, and checksums are in place. The Homebrew formula is intentionally not advertised until the first release publishes it.

### Required before calling it production-ready

1. **Platform runtime hardening:** exercise native event shapes, renames, root loss, watch limits, Git worktrees, and high-volume activity on Linux and Windows—not only compilation and short CI tests.
2. **Field evidence:** CI covers native smoke behavior, but interactive Linux and Windows sessions still need sustained high-volume and terminal testing.
3. **Channel failure recovery:** individual watches recover, but a fully disconnected callback channel still requires restart.
4. **Release compatibility policy:** document supported OS versions and test a declared minimum Rust version before promising one beyond Rust 1.88.

## Distribution

### Available now: Git install

This remains available before the first tagged release:

```sh
cargo install --git https://github.com/juanandresgs/FluxCapacitor --locked
```

It is suitable for early testers but not reproducible unless users pin `--rev` or install from a tag.

### Prepared for `v0.1.0`

Pushing a matching version tag runs the generated `dist` workflow and publishes:

- Native archives for Apple Silicon and Intel macOS, x86-64 and ARM64 Linux, and x86-64 Windows.
- SHA-256 checksums and a unified checksum manifest.
- Shell and PowerShell installers hosted on the GitHub release.
- A generated `flux` formula published to `juanandresgs/homebrew-tap`.

The generated Homebrew formula installs the release archives rather than compiling Rust source. The tap is intentionally personal rather than `homebrew/core` while Flux remains an alpha.

Publishing to crates.io remains optional; it is no longer required to give non-Rust users a low-friction installation path.

## Recommended sequence

1. Confirm CI and the release workflow's pull-request plan are green.
2. Tag `v0.1.0` as an alpha release.
3. Test each published installation path on its native platform.
4. Add binary and Homebrew instructions to the README only after those checks pass.
5. Gather external usage before expanding platform promises or pursuing `homebrew/core`.
