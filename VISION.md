# marchiver vision

marchiver is a one-file-at-a-time CLI for inspecting, planning, transcoding,
verifying, and safely migrating media. It writes versioned Pkl plans and
manifests, performs an exact-copy or explicit AV1 archive profile, verifies the
result by full decode, and only then moves the source out of the way. It exists
so that archiving a media library is an auditable transaction rather than a
sequence of `mv` and `ffmpeg` invocations.

## Problem

Media migration is where libraries get damaged. A re-encode may be lossy in
ways nobody checked, a copy may finish before the data is durable, an interrupted
run leaves a half-written file next to the original, and "move to the archive"
silently leaves no record of what was verified. Host-specific storage policies
make the same operation behave differently on each machine, and interactive
tools make it hard to resume.

## Principles

- **Transactions, not steps.** `inspect`, `plan`, `apply`, `verify`, and
  `restore` are separate commands over a versioned plan, with a crash-resume
  journal behind them.
- **Verify before disposition.** The target is checked by a full decode and the
  source is quarantined only after a durable manifest exists.
- **Quarantine is reversible.** The source is removed by a same-filesystem
  hard-link and unlink, so the media bytes are not rewritten and `restore` can
  bring it back without overwriting an existing source. Policy can select
  `preserve` or `delete` instead, from trusted configuration.
- **Encoding is explicit.** The default `copy` profile proves an exact SHA-256
  copy; `--profile av1` is an opt-in SVT-AV1 encode at fixed CRF 20/preset 6 in
  Matroska, copying other streams, metadata, and chapters.
- **No host-specific storage policy.** The tool describes one file's transition
  and leaves placement to the operator.
- **Pkl inputs are code.** Plans and manifests are executable configuration and
  must come from a trusted local source; the documentation says so plainly.

## Scope

- inspection of media streams and metadata
- plan generation and application with resumable journals
- exact-copy and AV1 archive profiles
- manifest-based verification and restore
- quarantine policy: hard-link/unlink, preserve, or delete
- Nix packaging that wraps the required FFmpeg build onto `PATH`

## Place in the fleet

No consumer was found in canix or the other owned repositories; the crate is
listed in canix's fleet Simit updater, so its generated flake and CI
infrastructure is refreshed with the fleet.
