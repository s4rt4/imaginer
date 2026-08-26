//! Uploading decoded pixels to the GPU.

use eframe::egui;
use imaginer_core::Decoded;
use imaginer_core::image::RgbaImage;

/// How image textures are sampled.
///
/// The magnification/minification split gives a viewer exactly the behaviour it
/// wants with no re-upload when zoom crosses 100%: past native size you want crisp
/// texels for pixel-peeping (nearest), below it you want them filtered. `mipmap_mode`
/// matters more than it looks — without it an 8000px image drawn at 10% aliases
/// badly, since the GPU point-samples a fraction of the texels.
const IMAGE_TEXTURE: egui::TextureOptions = egui::TextureOptions {
    magnification: egui::TextureFilter::Nearest,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: Some(egui::TextureFilter::Linear),
};

/// An image living on the GPU, plus what had to be done to get it there.
pub struct ImageTexture {
    pub handle: egui::TextureHandle,
    /// Size actually uploaded, in texels — smaller than `source_size` for a preview
    /// or an image that exceeded the GPU's limit.
    pub uploaded_size: (u32, u32),
    /// Native size of the source image.
    pub source_size: (u32, u32),
    /// Set when the image was too large for the GPU and had to be scaled down.
    pub downscaled: bool,
}

impl ImageTexture {
    /// Native size as a vector, used to lay the image out at 100% zoom.
    pub fn source_vec(&self) -> egui::Vec2 {
        egui::vec2(self.source_size.0 as f32, self.source_size.1 as f32)
    }
}

pub fn upload(ctx: &egui::Context, name: &str, decoded: &Decoded) -> ImageTexture {
    // `full_size` rather than the decoded size: a preview is a stand-in for an image
    // whose real dimensions are already known, and laying it out at its own small
    // size would make the view jump when the full decode swapped in.
    upload_sized(ctx, name, &decoded.pixels, decoded.full_size)
}

/// Upload pixels that are their own source — the output of the edit pipeline, which
/// has no larger original behind it.
pub fn upload_pixels(ctx: &egui::Context, name: &str, pixels: &RgbaImage) -> ImageTexture {
    upload_sized(ctx, name, pixels, pixels.dimensions())
}

fn upload_sized(
    ctx: &egui::Context,
    name: &str,
    pixels: &RgbaImage,
    source_size: (u32, u32),
) -> ImageTexture {
    let max_side = ctx.input(|i| i.max_texture_side) as u32;
    let (width, height, bytes) = imaginer_core::decode::for_upload(pixels, max_side);

    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &bytes);

    ImageTexture {
        handle: ctx.load_texture(name, image, IMAGE_TEXTURE),
        uploaded_size: (width, height),
        source_size,
        downscaled: (width, height) != pixels.dimensions(),
    }
}

/// Replace what a texture shows with new pixels of the same source size.
///
/// The animation path uses this once per displayed frame. `TextureHandle::set`
/// reuses the GPU texture rather than allocating a new one per frame — an
/// animation that allocated on every advance would grow egui's texture manager
/// for as long as it played.
pub fn set_pixels(ctx: &egui::Context, texture: &mut ImageTexture, pixels: &RgbaImage) {
    debug_assert_eq!(
        pixels.dimensions(),
        texture.source_size,
        "animation frames are composited to one canvas size; a size change means a new image"
    );

    let max_side = ctx.input(|i| i.max_texture_side) as u32;
    let (width, height, bytes) = imaginer_core::decode::for_upload(pixels, max_side);
    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &bytes);
    texture.handle.set(image, IMAGE_TEXTURE);
}
