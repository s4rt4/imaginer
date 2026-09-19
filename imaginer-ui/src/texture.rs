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
    /// Whether the picture has any see-through pixels, which is what earns it a
    /// checkerboard behind it. Decided on the decode thread, not here.
    pub has_transparency: bool,
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
    let mut texture = upload_sized(ctx, name, &decoded.pixels, decoded.full_size);
    texture.has_transparency = decoded.has_transparency;
    texture
}

/// Upload pixels that are their own source — the output of the edit pipeline, which
/// has no larger original behind it.
pub fn upload_pixels(ctx: &egui::Context, name: &str, pixels: &RgbaImage) -> ImageTexture {
    let mut texture = upload_sized(ctx, name, pixels, pixels.dimensions());
    // Unlike the decode path this runs on the UI thread, so the answer comes
    // from a stride-four scan — one alpha byte in sixteen. An edit that
    // introduced sparsely-distributed transparency could be missed; the
    // checkerboard is cosmetic and the next full decode corrects it.
    texture.has_transparency = pixels
        .as_raw()
        .chunks_exact(4)
        .step_by(4)
        .any(|px| px[3] != 255);
    texture
}

/// What is drawn behind see-through pixels, and how big one repeat of it is.
///
/// Screen-anchored by construction: the viewer stretches one draw call across the
/// image rectangle with UVs that run past 1.0 and `wrap_mode: Repeat`, so the
/// pattern stays a fixed size on screen while the picture pans and zooms under
/// it — exactly how Photoshop's transparency grid behaves. `period` is what the
/// viewer divides by to get those UVs, and it belongs here because only this
/// module knows how many texels a tile is.
pub struct Backdrop {
    pub handle: egui::TextureHandle,
    /// Points that one full tile covers on screen.
    pub period: f32,
}

const LIGHT: u8 = 0x3c;
const DARK: u8 = 0x2c;

/// The ordinary transparency grid: a two-by-two checker of 8pt squares.
pub fn checkerboard(ctx: &egui::Context) -> Backdrop {
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [2, 2],
        &[
            LIGHT, LIGHT, LIGHT, 255, DARK, DARK, DARK, 255, DARK, DARK, DARK, 255, LIGHT, LIGHT,
            LIGHT, 255,
        ],
    );
    Backdrop {
        handle: ctx.load_texture(
            "transparency-checker",
            image,
            egui::TextureOptions {
                magnification: egui::TextureFilter::Nearest,
                minification: egui::TextureFilter::Nearest,
                wrap_mode: egui::TextureWrapMode::Repeat,
                mipmap_mode: None,
            },
        ),
        period: 2.0 * 8.0,
    }
}

/// The same tones as diagonal bands, for artwork that is drawn rather than sampled.
///
/// Same two greys and about the same band width as the checker's squares, so it
/// sits behind a picture exactly as quietly. Only the direction is different, and
/// direction is what the eye reads without being asked to compare — which is the
/// whole job here: it says *this file is vector* at a glance, and says nothing
/// else. A viewer that changed the tones instead would be changing the surround
/// every colour on screen is judged against.
///
/// Sampled linearly, unlike the checker. A 45-degree edge across whole texels is
/// a staircase, and at this size the only thing linear softens is that staircase.
pub fn diagonals(ctx: &egui::Context) -> Backdrop {
    const SIDE: usize = 16;
    /// Points per texel. Sixteen of them is the tile; a band is eight across,
    /// which measured square to itself is the checker's 8pt.
    const TEXEL: f32 = 1.5;

    Backdrop {
        handle: ctx.load_texture(
            "transparency-diagonals",
            egui::ColorImage::from_rgba_unmultiplied([SIDE, SIDE], &diagonal_pixels(SIDE)),
            egui::TextureOptions {
                magnification: egui::TextureFilter::Linear,
                minification: egui::TextureFilter::Linear,
                wrap_mode: egui::TextureWrapMode::Repeat,
                mipmap_mode: None,
            },
        ),
        period: SIDE as f32 * TEXEL,
    }
}

/// The band pattern itself, as RGBA.
///
/// Constant along an anti-diagonal, so the bands run from bottom-left to top
/// right. The band width divides the side exactly, which is what lets a tile
/// meet its own neighbour with no seam.
fn diagonal_pixels(side: usize) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(side * side * 4);
    for y in 0..side {
        for x in 0..side {
            let tone = if (x + y) % side < side / 2 {
                LIGHT
            } else {
                DARK
            };
            pixels.extend_from_slice(&[tone, tone, tone, 255]);
        }
    }
    pixels
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
        has_transparency: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(pixels: &[u8], side: usize, x: usize, y: usize) -> u8 {
        pixels[(y * side + x) * 4]
    }

    #[test]
    fn the_bands_are_diagonal_and_seamless() {
        const SIDE: usize = 16;
        let pixels = diagonal_pixels(SIDE);

        for y in 0..SIDE {
            for x in 0..SIDE {
                // Each row is the one above shifted by one texel: that shift is
                // what a 45-degree band *is*, and doing it modulo the side is
                // what makes the tile join itself.
                assert_eq!(
                    tone(&pixels, SIDE, x, y),
                    tone(&pixels, SIDE, (x + 1) % SIDE, (y + SIDE - 1) % SIDE),
                    "texel {x},{y} breaks the diagonal"
                );
            }
        }
    }

    #[test]
    fn the_two_tones_are_evenly_split() {
        // Uneven bands would read as a pattern on top of the picture rather than
        // as a neutral surround, which is the one thing a backdrop must not do.
        const SIDE: usize = 16;
        let pixels = diagonal_pixels(SIDE);
        let light = pixels.chunks_exact(4).filter(|px| px[0] == LIGHT).count();

        assert_eq!(light, SIDE * SIDE / 2);
    }
}
