# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Breaking

- `FlagsLayer` no longer reads `X-Feature-Flags` or sets `X-Enabled-Flags` by default, since
  any caller could probe flag states. Opt in with `FlagsLayer::expose_header(true)`; at most
  50 flag names are evaluated per request. The client is still added to request extensions.
- `ClientBuilder::build` rejects non-`https` base URLs, except `http` on `localhost`,
  `127.0.0.1` and `[::1]`.
- Removed the unused optional `rusqlite` dependency (and its implicit `rusqlite` feature).

### Fixed

- A cancelled flag check (e.g. a dropped request future) could leave the refresh lock held
  forever, so flags never refreshed again. The lock is now released on drop.
- When the API fails after retries, the last-known-good flags are kept instead of being
  replaced with env-only flags, and the circuit breaker now opens for 30s.
- `X-Enabled-Flags` was dropped when the inner service returned `Pending` after flags resolved.
- Server-supplied `intervalAllowed` is clamped to 10..=3600 seconds (non-positive becomes 60).
- API responses larger than 5 MB are rejected.
- Project/agent/environment IDs are redacted from `Auth`'s `Debug` output, `debug_info()`
  and auth error messages.

### Deprecated

- `ClientBuilder::with_file_name`, which never had any effect.

## [0.1.5](https://github.com/flags-gg/flags-rs/compare/v0.1.4...v0.1.5) - 2025-05-19

### Other

- Merge branch 'main' of github.com:flags-gg/flags-rs
