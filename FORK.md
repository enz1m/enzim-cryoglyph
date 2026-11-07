# Fork provenance

Upstream: https://github.com/iced-rs/cryoglyph

Base: `b4576782d7c22ecef973d32cf5b18881ad14fe89` (crate 0.1.0).
The installed enzUI fork originally used `53ba3e879539d19ed8162942126a977ec896cc3b`
and already ported upstream's generation tracker (`f4e7e4e`). Using b457678
preserves that change's original history and authorship. Its remaining two
commits only route cosmic-text, superseded by our explicit public pin.
No wgpu 30 or etagere 0.3 update is included.

The separate signed fork commits cover:

1. Public, full-revision wgpu and cosmic-text dependencies. Native development
   enables Vulkan without imposing it on library consumers.
2. An lru 0.18.2 minimum, retaining default-features = false.
3. GPU texture copies on atlas growth, COPY_SRC usage, and statistics.
4. Atlas growth caps, clamped to the adapter limit and allocated size.
5. Extraction of the existing eviction loop and six CPU regression tests.
6. Combined glyph/baseline rounding and fractional-placement regression coverage.
7. Repository metadata and documentation.

The runtime source matches the installed enzUI implementation after formatting
and modification notices. The original examples, benchmarks and text samples
remain available. The crate name, upstream authors and all three licenses remain.
Modified Rust files carry a notice referring here; history identifies each change.

This publication prepares a standalone source dependency. Existing framework
and application dependency declarations are not changed.
