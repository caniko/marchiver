# marchiver

<!-- simit:badges:start -->

[![CI](https://img.shields.io/badge/CI-managed+extra-2088ff)](.github/workflows/ci.yaml) [![Nix](https://img.shields.io/badge/Nix-managed-5277c3)](flake.nix) [![crates.io](https://img.shields.io/badge/crates.io-ready-f46623)](https://crates.io/crates/marchiver) [![artifacts](https://img.shields.io/badge/artifacts-configured-2ea44f)](.github/workflows/release.yml)

<!-- simit:badges:end -->

`marchiver` is a one-file-at-a-time CLI for inspecting, planning, transcoding,
verifying, and safely migrating media without host-specific storage policy.

It writes versioned Pkl plans and manifests. Pkl inputs are executable
configuration and must come from a trusted local source.

## Usage

```sh
marchiver inspect movie.mkv --output inspection.pkl
marchiver plan movie.mkv /archive/movie.mkv --output plan.pkl
marchiver apply plan.pkl
marchiver verify /archive/movie.mkv.marchiver.pkl
```

The default `copy` profile verifies an exact SHA-256 copy. Use `--profile av1`
to encode eligible video streams with SVT-AV1 at CRF 20/preset 6 in Matroska
while copying other streams, metadata, and chapters.

After a durable manifest and full decode check, the source is quarantined by a
same-filesystem hard-link and unlink. This does not rewrite the media bytes.
`marchiver restore MANIFEST` reverses quarantine without overwriting an
existing source. Trusted Pkl configuration can select `preserve` or `delete`
instead.

FFmpeg must provide `ffmpeg`, `ffprobe`, and `libsvtav1`. The Nix package wraps
the required FFmpeg build onto `PATH`.

## Development

With Nix:

```sh
nix develop
cargo test
cargo clippy --all-targets --all-features -- --deny warnings
```

Coverage uses LLVM instrumentation because rs-harbor's development shell
defaults to Cranelift:

```sh
nix develop -c env CARGO_PROFILE_DEV_CODEGEN_BACKEND=llvm \
cargo llvm-cov --all-features --workspace --summary-only --fail-under-lines 80
```

Without Nix, Rust 1.88 or newer and FFmpeg are required.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option. See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).
