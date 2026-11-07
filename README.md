# enzim-cryoglyph

The enzUI fork of [iced-rs/cryoglyph](https://github.com/iced-rs/cryoglyph),
a GPU text renderer built on wgpu and cosmic-text. The Rust crate remains
`cryoglyph`; the repository name distinguishes this fork from upstream.

Changes include GPU copies on atlas growth, growth limits and statistics,
cache-eviction regression tests, and combined baseline rounding.
Upstream history, authorship and licenses are preserved.
See [FORK.md](FORK.md), [validation](VALIDATION.md) and the
[original upstream README](README.upstream.md).

## Build

```sh
cargo +1.95.0 check --all-targets
cargo +1.95.0 test --lib
# Explicit GPU checks: Vulkan adapter and installed sans-serif font required.
cargo +1.95.0 test --lib -- --include-ignored --test-threads=1
```

Dependencies use public Git sources with full revision pins:

| Dependency | Source revision |
| --- | --- |
| [enzim-wgpu](https://github.com/enz1m/enzim-wgpu) | `7f040c8501f6838262c90ede4edfa40940fe2de8` |
| [enzim-cosmic-text](https://github.com/enz1m/enzim-cosmic-text) | `ad474131e52c0fde5979f8def2889489b50a0480` |

Applications using these types directly must use the same Git URL and revision
to keep Rust types compatible. Pin this repository to a full commit too, and
commit the application's Cargo.lock. This library follows upstream in leaving
Cargo.lock untracked; transitive registry dependencies resolve for each consumer.

## License

MIT OR Apache-2.0 OR Zlib, as upstream. Original license files and notices remain.
