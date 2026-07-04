# Production Readiness

Assessment date: July 3, 2026

## Verdict

Flux is ready to share publicly as an **alpha developer tool**, not as a production-grade audit trail or stable cross-platform utility.

The core architecture is appropriate for the product: native event streams, conservative classification, bounded retained history, explicit integrity events, and no unsupported agent/process attribution. Local macOS behavior is strong. The main readiness gaps are release engineering, platform evidence, overload behavior, and recovery—not the basic interaction model.

## Evidence

- `cargo test --locked`: 25 tests pass locally, including runtime linked-worktree discovery, submodule checkout resolution, incremental-baseline honesty, dynamic native root registration, and real multi-root filesystem and Git activity.
- `cargo clippy --locked --all-targets -- -D warnings`: passes.
- `cargo fmt --check`: passes.
- `cargo audit`: no known RustSec advisories in the locked dependency graph.
- `cargo publish --dry-run`: packages and rebuilds successfully.
- Package size: approximately 43 KiB compressed source archive.
- GitHub Actions: the Rust 1.88 test suite passes on current Ubuntu, macOS, and Windows runners; formatting, Clippy, and package verification pass on Ubuntu.
- Local runtime testing: macOS on Apple Silicon.
- Repository state at assessment: private, no tags, no releases, and no published crate.

## Release gates

### Required before public sharing

1. **Make the repository public.** Installation from Git and source browsing otherwise remain unavailable to others.
2. **Tag the first release.** Use an explicit alpha version such as `v0.1.0`; do not present it as stable.

The missing MIT license file and Cargo release metadata were corrected during this assessment.

### Required before calling it production-ready

1. **Platform runtime hardening:** exercise native event shapes, renames, root loss, watch limits, Git worktrees, and high-volume activity on Linux and Windows—not only compilation and short CI tests.
2. **Backpressure:** filesystem and Git callback channels are unbounded. Rendering yields after 512 filesystem events, which preserves responsiveness, but a producer can still grow memory without a hard queue limit.
3. **Recovery:** a lost watcher or dropped-event sentinel is reported honestly, but Flux does not re-establish observation or reconcile current state.
4. **Per-root integrity:** global health can obscure which workspace has degraded or stopped.
5. **Terminal cleanup:** panic and normal exits restore the terminal, but termination signals and failures between raw-mode activation and terminal construction are not comprehensively guarded.
6. **Release compatibility policy:** document supported OS versions and test a declared minimum Rust version before promising one beyond the dependency-derived Rust 1.88 floor.

## Distribution recommendation

### Now: Git install

After making the repository public, this is immediately available with no release infrastructure:

```sh
cargo install --git https://github.com/juanandresgs/FluxCapacitor
```

It is suitable for early testers but not reproducible unless users pin `--rev` or install from a tag.

### First lightweight release: crates.io

Recommended. The package already passes Cargo's dry-run verification, and users would install it with:

```sh
cargo install flux-capacitor
```

The package name is `flux-capacitor`; the installed binary is `flux`. Publishing requires a public repository, crates.io ownership/login, a release tag, and a one-time `cargo publish`. Add a changelog only when releases become recurring work.

### Optional: GitHub release binaries

Useful if non-Rust users ask for installation. A tag-triggered workflow can build archives for macOS, Linux, and Windows. This is more maintenance than crates.io because target coverage, archive naming, checksums, and runtime testing become release responsibilities.

### Not now: Homebrew

Do not pursue `homebrew/core`: the project has no stable public release, user adoption, or broad platform evidence. A personal tap is possible, but it adds a separate repository/formula and ongoing checksum/version maintenance. That violates the “no uplift unless worthwhile” preference while `cargo install` is already viable.

## Recommended sequence

1. Push the license, metadata, README, and CI changes.
2. Make the GitHub repository public.
3. Confirm the three-OS CI matrix is green.
4. Tag `v0.1.0` as an alpha release.
5. Publish to crates.io only if a low-friction installer is wanted.
6. Gather external usage before investing in Homebrew or binary releases.
