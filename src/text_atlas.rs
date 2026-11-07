// Modified by enzUI contributors. See FORK.md.
use crate::{Cache, CacheKey, GlyphDetails, text_render::ContentType};
use etagere::{Allocation, BucketedAtlasAllocator, size2};
use lru::LruCache;
use rustc_hash::FxHasher;
use std::hash::BuildHasherDefault;
use wgpu::{
    BindGroup, CommandEncoderDescriptor, DepthStencilState, Device, Extent3d, MultisampleState,
    Origin3d, Queue, RenderPipeline, TexelCopyTextureInfo, Texture, TextureAspect,
    TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureView,
    TextureViewDescriptor,
};

type Hasher = BuildHasherDefault<FxHasher>;

/// Instrumentation counters for a [`TextAtlas`].
///
/// Atlas growth used to re-rasterize and re-upload every
/// cached glyph on the CPU. These counters make the mechanism observable, so a
/// gate can assert that growth costs zero rasterizations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AtlasStatistics {
    /// Glyphs rasterized (`SwashCache::get_image_uncached`) while preparing
    /// text. Only glyphs that were not already in the atlas are counted.
    pub glyphs_rasterized: u64,
    /// Of those, the glyphs whose bitmap was empty (a space, say) and so were
    /// never uploaded. `glyphs_rasterized - glyphs_without_pixels` is the exact
    /// number of glyph uploads a correct atlas performs.
    pub glyphs_without_pixels: u64,
    /// Glyphs rasterized *again* because the atlas grew. This must stay `0`.
    pub glyphs_rerasterized_on_growth: u64,
    /// `Queue::write_texture` calls made to upload glyph bitmaps.
    pub glyph_queue_writes: u64,
    /// Bytes uploaded through `Queue::write_texture` for glyph bitmaps.
    pub glyph_queue_write_bytes: u64,
    /// Successful atlas growths (one per atlas kind).
    pub growths: u64,
    /// Growth attempts refused because the atlas is already at the adapter's
    /// `max_texture_dimension_2d`. A non-zero value means text may be dropped.
    pub growth_failures: u64,
    /// GPU texture-to-texture copies performed to preserve grown atlases.
    pub texture_copies: u64,
    /// Bytes preserved by GPU texture-to-texture copies during growth.
    pub texture_copy_bytes: u64,
    /// Current side length of the mask (monochrome) atlas, in texels.
    pub mask_size: u32,
    /// Current side length of the color atlas, in texels.
    pub color_size: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct InnerStatistics {
    pub glyphs_rasterized: u64,
    pub glyphs_without_pixels: u64,
    pub glyphs_rerasterized_on_growth: u64,
    pub glyph_queue_writes: u64,
    pub glyph_queue_write_bytes: u64,
    pub growths: u64,
    pub growth_failures: u64,
    pub texture_copies: u64,
    pub texture_copy_bytes: u64,
}

#[allow(dead_code)]
pub(crate) struct InnerAtlas {
    pub kind: Kind,
    pub texture: Texture,
    pub texture_view: TextureView,
    pub packer: BucketedAtlasAllocator,
    pub size: u32,
    pub glyph_cache: LruCache<CacheKey, GlyphDetails, Hasher>,
    /// Frame counter bumped by `trim`. A glyph whose `last_used` equals it was
    /// touched since the last trim and is in use this frame. It replaces a
    /// per-frame `HashSet` of in-use keys (upstream cryoglyph f4e7e4e).
    pub generation: usize,
    pub max_texture_dimension_2d: u32,
    pub stats: InnerStatistics,
}

impl InnerAtlas {
    const INITIAL_SIZE: u32 = 256;

    fn new(device: &Device, _queue: &Queue, kind: Kind) -> Self {
        let max_texture_dimension_2d = device.limits().max_texture_dimension_2d;
        let size = Self::INITIAL_SIZE.min(max_texture_dimension_2d);

        let packer = BucketedAtlasAllocator::new(size2(size as i32, size as i32));

        // Create a texture to use for our atlas
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("glyphon atlas"),
            size: Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: kind.texture_format(),
            // COPY_SRC so growth can blit the old atlas into
            // the larger one on the GPU instead of rasterizing it again.
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let texture_view = texture.create_view(&TextureViewDescriptor::default());

        let glyph_cache = LruCache::unbounded_with_hasher(Hasher::default());

        Self {
            kind,
            texture,
            texture_view,
            packer,
            size,
            glyph_cache,
            generation: 0,
            max_texture_dimension_2d,
            stats: InnerStatistics::default(),
        }
    }

    pub(crate) fn try_allocate(&mut self, width: usize, height: usize) -> Option<Allocation> {
        let size = size2(width as i32, height as i32);

        loop {
            let allocation = self.packer.allocate(size);

            if allocation.is_some() {
                return allocation;
            }

            // Try to free least recently used allocation
            let (_, mut value) = self.glyph_cache.peek_lru()?;

            // Find a glyph with an actual size
            while value.atlas_id.is_none() {
                // All sized glyphs are in use, cache is full
                if value.last_used == self.generation {
                    return None;
                }

                let _ = self.glyph_cache.pop_lru();

                (_, value) = self.glyph_cache.peek_lru()?;
            }

            // All sized glyphs are in use, cache is full
            if value.last_used == self.generation {
                return None;
            }

            let (_, value) = self.glyph_cache.pop_lru().unwrap();
            self.packer.deallocate(value.atlas_id.unwrap());
        }
    }

    pub fn num_channels(&self) -> usize {
        self.kind.num_channels()
    }

    /// Grows the atlas, preserving every already-cached glyph with a single GPU
    /// texture-to-texture copy.
    ///
    /// This used to allocate the replacement texture and then
    /// walk `glyph_cache`, calling `SwashCache::get_image_uncached` and
    /// `Queue::write_texture` once per cached glyph — an unbounded main-thread
    /// CPU and upload spike at every 256 -> 512 -> 1024 ... boundary. The packer
    /// grows in place and keeps every existing allocation at its current
    /// coordinates, so copying the old `size x size` region to the origin of the
    /// new texture reproduces the old contents exactly. It takes neither a
    /// `FontSystem` nor a `SwashCache`, so growth *cannot* rasterize.
    pub(crate) fn grow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        if self.size >= self.max_texture_dimension_2d {
            self.stats.growth_failures += 1;
            return false;
        }

        // Grow each dimension by a factor of 2. The growth factor was chosen to match the growth
        // factor of `Vec`.`
        const GROWTH_FACTOR: u32 = 2;
        let new_size = (self.size * GROWTH_FACTOR).min(self.max_texture_dimension_2d);

        self.packer.grow(size2(new_size as i32, new_size as i32));

        // Create a texture to use for our atlas
        let new_texture = device.create_texture(&TextureDescriptor {
            label: Some("glyphon atlas"),
            size: Extent3d {
                width: new_size,
                height: new_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: self.kind.texture_format(),
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let old_texture = std::mem::replace(&mut self.texture, new_texture);
        let old_size = self.size;

        // Preserve the cached glyphs with one GPU copy. Queue writes staged
        // earlier this frame are flushed before the command buffers of the next
        // submission, so glyphs uploaded moments ago are included. `old_texture`
        // is dropped below, but wgpu keeps the resource alive until the
        // submission that reads it has completed.
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("cryoglyph atlas growth copy"),
        });

        encoder.copy_texture_to_texture(
            TexelCopyTextureInfo {
                texture: &old_texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            Extent3d {
                width: old_size,
                height: old_size,
                depth_or_array_layers: 1,
            },
        );

        queue.submit(Some(encoder.finish()));

        self.stats.growths += 1;
        self.stats.texture_copies += 1;
        self.stats.texture_copy_bytes +=
            u64::from(old_size) * u64::from(old_size) * self.kind.num_channels() as u64;

        self.texture_view = self.texture.create_view(&TextureViewDescriptor::default());
        self.size = new_size;

        true
    }

    fn trim(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Mask,
    Color { srgb: bool },
}

impl Kind {
    fn num_channels(self) -> usize {
        match self {
            Kind::Mask => 1,
            Kind::Color { .. } => 4,
        }
    }

    fn texture_format(self) -> wgpu::TextureFormat {
        match self {
            Kind::Mask => TextureFormat::R8Unorm,
            Kind::Color { srgb } => {
                if srgb {
                    TextureFormat::Rgba8UnormSrgb
                } else {
                    TextureFormat::Rgba8Unorm
                }
            }
        }
    }
}

/// The color mode of an [`Atlas`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Accurate color management.
    ///
    /// This mode will use a proper sRGB texture for colored glyphs. This will
    /// produce physically accurate color blending when rendering.
    Accurate,

    /// Web color management.
    ///
    /// This mode reproduces the color management strategy used in the Web and
    /// implemented by browsers.
    ///
    /// This entails storing glyphs colored using the sRGB color space in a
    /// linear RGB texture. Blending will not be physically accurate, but will
    /// produce the same results as most UI toolkits.
    ///
    /// This mode should be used to render to a linear RGB texture containing
    /// sRGB colors.
    Web,
}

/// An atlas containing a cache of rasterized glyphs that can be rendered.
pub struct TextAtlas {
    cache: Cache,
    pub(crate) bind_group: BindGroup,
    pub(crate) color_atlas: InnerAtlas,
    pub(crate) mask_atlas: InnerAtlas,
    pub(crate) format: TextureFormat,
    pub(crate) color_mode: ColorMode,
}

impl TextAtlas {
    /// Creates a new [`TextAtlas`].
    pub fn new(device: &Device, queue: &Queue, cache: &Cache, format: TextureFormat) -> Self {
        Self::with_color_mode(device, queue, cache, format, ColorMode::Accurate)
    }

    /// Creates a new [`TextAtlas`] with the given [`ColorMode`].
    pub fn with_color_mode(
        device: &Device,
        queue: &Queue,
        cache: &Cache,
        format: TextureFormat,
        color_mode: ColorMode,
    ) -> Self {
        let color_atlas = InnerAtlas::new(
            device,
            queue,
            Kind::Color {
                srgb: match color_mode {
                    ColorMode::Accurate => true,
                    ColorMode::Web => false,
                },
            },
        );
        let mask_atlas = InnerAtlas::new(device, queue, Kind::Mask);

        let bind_group = cache.create_atlas_bind_group(
            device,
            &color_atlas.texture_view,
            &mask_atlas.texture_view,
        );

        Self {
            cache: cache.clone(),
            bind_group,
            color_atlas,
            mask_atlas,
            format,
            color_mode,
        }
    }

    pub fn trim(&mut self) {
        self.mask_atlas.trim();
        self.color_atlas.trim();
    }

    /// Returns the [`AtlasStatistics`] accumulated since creation (or since the
    /// last [`TextAtlas::reset_statistics`]).
    pub fn statistics(&self) -> AtlasStatistics {
        let mask = &self.mask_atlas.stats;
        let color = &self.color_atlas.stats;

        AtlasStatistics {
            glyphs_rasterized: mask.glyphs_rasterized + color.glyphs_rasterized,
            glyphs_without_pixels: mask.glyphs_without_pixels + color.glyphs_without_pixels,
            glyphs_rerasterized_on_growth: mask.glyphs_rerasterized_on_growth
                + color.glyphs_rerasterized_on_growth,
            glyph_queue_writes: mask.glyph_queue_writes + color.glyph_queue_writes,
            glyph_queue_write_bytes: mask.glyph_queue_write_bytes + color.glyph_queue_write_bytes,
            growths: mask.growths + color.growths,
            growth_failures: mask.growth_failures + color.growth_failures,
            texture_copies: mask.texture_copies + color.texture_copies,
            texture_copy_bytes: mask.texture_copy_bytes + color.texture_copy_bytes,
            mask_size: self.mask_atlas.size,
            color_size: self.color_atlas.size,
        }
    }

    /// Resets the counters returned by [`TextAtlas::statistics`].
    pub fn reset_statistics(&mut self) {
        self.mask_atlas.stats = InnerStatistics::default();
        self.color_atlas.stats = InnerStatistics::default();
    }

    pub(crate) fn grow(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        content_type: ContentType,
    ) -> bool {
        let did_grow = match content_type {
            ContentType::Mask => self.mask_atlas.grow(device, queue),
            ContentType::Color => self.color_atlas.grow(device, queue),
        };

        if did_grow {
            self.rebind(device);
        }

        did_grow
    }

    pub(crate) fn inner_for_content_mut(&mut self, content_type: ContentType) -> &mut InnerAtlas {
        match content_type {
            ContentType::Color => &mut self.color_atlas,
            ContentType::Mask => &mut self.mask_atlas,
        }
    }

    pub(crate) fn get_or_create_pipeline(
        &self,
        device: &Device,
        multisample: MultisampleState,
        depth_stencil: Option<DepthStencilState>,
    ) -> RenderPipeline {
        self.cache
            .get_or_create_pipeline(device, self.format, multisample, depth_stencil)
    }

    fn rebind(&mut self, device: &wgpu::Device) {
        self.bind_group = self.cache.create_atlas_bind_group(
            device,
            &self.color_atlas.texture_view,
            &self.mask_atlas.texture_view,
        );
    }
}

#[cfg(test)]
mod gpu_tests {
    use super::*;

    fn gpu() -> (Device, Queue) {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("GPU tests require a Vulkan adapter");
        pollster::block_on(adapter.request_device(&Default::default())).unwrap()
    }

    fn read_origin(device: &Device, queue: &Queue, atlas: &InnerAtlas) -> Vec<u8> {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            atlas.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let (send, recv) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                send.send(result).unwrap();
            });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        recv.recv().unwrap().unwrap();
        let mapped = buffer.slice(..).get_mapped_range();
        let row_bytes = 4 * atlas.num_channels();
        let result = mapped
            .chunks(256)
            .flat_map(|row| row[..row_bytes].iter().copied())
            .collect();
        drop(mapped);
        buffer.unmap();
        result
    }

    #[test]
    #[ignore = "requires a Vulkan adapter; run explicitly"]
    fn growth_preserves_mask_and_color_texels() {
        let (device, queue) = gpu();
        for kind in [Kind::Mask, Kind::Color { srgb: true }] {
            let mut atlas = InnerAtlas::new(&device, &queue, kind);
            let pixels: Vec<u8> = (0..16 * atlas.num_channels())
                .map(|i| (i * 3 + 17) as u8)
                .collect();
            queue.write_texture(
                atlas.texture.as_image_copy(),
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * atlas.num_channels() as u32),
                    rows_per_image: None,
                },
                Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            );
            assert_eq!(read_origin(&device, &queue, &atlas), pixels);
            assert!(atlas.grow(&device, &queue));
            assert_eq!(atlas.size, 512);
            assert_eq!(read_origin(&device, &queue, &atlas), pixels);
            assert_eq!(atlas.stats.texture_copies, 1);
            assert_eq!(atlas.stats.glyphs_rerasterized_on_growth, 0);
        }
    }
}
