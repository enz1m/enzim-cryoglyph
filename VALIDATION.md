# Validation

Linux x86_64, Rust 1.95.0, native Vulkan adapter.

- `cargo check --locked --all-targets` passes, including upstream example and benchmark.
- `cargo fmt --all -- --check` passes.
- All nine library tests pass when explicitly including GPU tests: six eviction
  cases, mask/color GPU-copy readback, cap clamping/refusal, and fractional baseline
  placement through the real text preparation path at 20 offsets.
- Removing the actual GPU copy produces a readback failure. Restoring the old
  double rounding produces a baseline failure. Changing LRU selection to MRU
  produces eviction failures. The restored implementation passes again.
- Cargo metadata resolves one wgpu/core/hal/types/naga family at the pinned public
  wgpu commit and one cosmic-text at its pinned public commit; no workspace paths.
- Runtime Rust code matches the installed fork after formatting and modification
  notices. Upstream README, examples, benchmark, samples and licenses are retained.
- Normal Clippy completes with warnings. Strict `-D warnings` fails on existing
  upstream type complexity in cache.rs and argument counts in text_render.rs;
  the upstream example also has needless-borrow warnings. These APIs are retained.

```sh
cargo +1.95.0 check --locked --all-targets
cargo +1.95.0 fmt --all -- --check
cargo +1.95.0 test --locked --lib -- --include-ignored --test-threads=1
cargo +1.95.0 clippy --locked --all-targets
```

The GPU tests are ignored by default so CPU-only builds remain usable; this
validation explicitly executed them. These checks do not claim visual review
of complete enzUI applications or execution on every GPU/backend.
