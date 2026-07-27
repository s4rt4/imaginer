//! Uploading decoded pixels to the GPU.

use eframe::egui;
use imaginer_core::Decoded;

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
    let max_side = ctx.input(|i| i.max_texture_side) as u32;
    let (width, height, bytes) = decoded.for_upload(max_side);

    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &bytes);

    ImageTexture {
        handle: ctx.load_texture(name, image, IMAGE_TEXTURE),
        uploaded_size: (width, height),
        source_size: decoded.full_size,
        downscaled: (width, height) != decoded.size(),
    }
}
