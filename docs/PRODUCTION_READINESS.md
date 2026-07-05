# Production Readiness

Assessment date: July 5, 2026

## Verdict

Flux is ready to share publicly as an **alpha developer tool**, not as a production-grade audit trail or stable cross-platform utility.

The core architecture is appropriate for the product: native event streams, conservative classification, bounded retained history, explicit integrity events, and no unsupported agent/process attribution. Local macOS behavior is strong. Automated release packaging is in place. The main readiness gaps are platform evidence, overload behavior, and recovery—not the basic interaction model.

## Evidence

- `cargo test`: 30 tests pass locally with one intentionally ignored live Beads mutation test; coverage includes runtime linked-worktree discovery, incremental-baseline honesty, Beads marker isolation, internal-metadata isolation, dynamic native root registration, real multi-root filesystem and Git activity, and semantic reads from Flux's own Beads history.
- The ignored live test was run explicitly and passed: a real `bd comment` produced native Dolt activity and rendered as a semantic `WORK` event in the release TUI.
- `cargo clippy --locked --all-targets -- -D warnings`: passes.
- `cargo fmt --check`: passes.
- `cargo audit`: no known RustSec advisories in the locked dependency graph.
- `cargo publish --dry-run`: packages and rebuilds successfully.
- Package size: approximately 43 KiB compressed source archive.
- GitHub Actions: the Rust 1.88 test suite passes on current Ubuntu, macOS, and Windows runners; formatting, Clippy, and package verification pass on Ubuntu.
- Local runtime testing: macOS on Apple Silicon.
- Repository state at assessment: public, with tag-triggered cross-platform release automation configured; no release tag has been published yet.

## Release gates

### Required before advertising binary installation

1. **Tag the first release.** Use `v0.1.0` and keep the project explicitly labeled alpha.
2. **Verify the release artifacts.** Install the generated macOS, Linux, and Windows archives on their native platforms and verify the shell, PowerShell, and Homebrew paths.

The public repository, MIT license, Cargo metadata, changelog, release workflow, installer generation, checksums, and Homebrew tap are in place.

### Required before calling it production-ready

1. **Platform runtime hardening:** exercise native event shapes, renames, root loss, watch limits, Git worktrees, and high-volume activity on Linux and Windows—not only compilation and short CI tests.
2. **Backpressure:** filesystem and Git callback channels are unbounded. Rendering yields after 512 visible filesystem events or 4,096 internal metadata triggers, which preserves responsiveness, but a producer can still grow memory without a hard queue limit.
3. **Recovery:** a lost watcher or dropped-event sentinel is reported honestly, but Flux does not re-establish observation or reconcile current state.
4. **Per-root integrity:** global health can obscure which workspace has degraded or stopped.
5. **Terminal cleanup:** panic and normal exits restore the terminal, but termination signals and failures between raw-mode activation and terminal construction are not comprehensively guarded.
6. **Release compatibility policy:** document supported OS versions and test a declared minimum Rust version before promising one beyond the dependency-derived Rust 1.88 floor.
7. **Beads compatibility policy:** WORK support currently targets embedded Beads 1.1/Dolt schemas and needs fixture coverage across supported Beads releases before it can be called stable.

## Distribution

### Available now: Git install

This remains available before the first tagged release:

```sh
cargo install --git https://github.com/juanandresgs/FluxCapacitor
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

1. Review and push the sparse README and generated release configuration.
2. Confirm CI and the release workflow's pull-request plan are green.
3. Tag `v0.1.0` as an alpha release.
4. Test each published installation path on its native platform.
5. Gather external usage before expanding platform promises or pursuing `homebrew/core`.
