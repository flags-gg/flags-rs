# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust client library for Flags.gg, a feature flag management service. It enables developers to control feature rollouts through remote flags with local override capabilities.

## Development Commands

```bash
# Run tests with all features
cargo test --all-features

# Build the library
cargo build

# Run the basic example
cargo run --example basic

# Check code without building
cargo check

# Format code
cargo fmt

# Run linter
cargo clippy
```

## Architecture

### Core Components

- **lib.rs**: Main client implementation with builder pattern
- **cache.rs**: Trait-based caching system supporting multiple backends
- **flag.rs**: Flag data structures and serialization

### Key Design Patterns

1. **Builder Pattern**: Client construction uses a fluent builder interface
2. **Trait-Based Cache**: Extensible caching through the `Cache` trait with async support
3. **Circuit Breaker**: Protects against API failures with automatic recovery
4. **Local Overrides**: Environment variables prefixed with `FLAGS_` override remote flags

### API Design

The client provides a fluent interface:
```rust
client.is("flag-name").enabled().await
client.list().await
```

### Error Handling

Uses a custom `FlagError` enum with graceful degradation when the API is unavailable. The client will fall back to cached values or defaults rather than failing completely.

## CI/CD

The repository uses GitHub Actions with:
- Automated testing on push to main
- Release automation via `release-plz`
- Automatic version bumping and changelog generation
- Publishing to crates.io

## Testing

Tests use mockito and wiremock for HTTP mocking. Some tests require serial execution using the `serial_test` crate.