# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Use authenticated Pklx 0.2.0 and patched TLS dependencies, removing the vulnerable ZIP-8 and QUIC dependency chains.
- Fetch public CI inputs over HTTPS without changing their locked sources.
- Evaluate Nix configuration from a checked sidecar without building Pklx during evaluation.
- Restrict Nix outputs to supported Linux and ARM64 macOS platforms, keeping musl tooling Linux-only.
- Expose the metadata-configured Attic publication app for native prebuilds.

## [0.1.0] - 2026-08-01

### Added

- Transactional `inspect`, `plan`, `apply`, `verify`, and `restore` commands.
- Versioned Pkl configuration, plans, manifests, and crash-resume journals.
- Exact-copy and explicit SVT-AV1 archive profiles with full decode checks.
- Same-filesystem source quarantine with preserve and delete policy options.
- Rust, Nix, GitHub Actions, Attic, and crates.io release infrastructure.
