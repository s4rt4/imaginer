//! Reading the flattened composite of a Photoshop `.psd` file.
//!
//! A viewer's promise is "what this file looks like", and a compatibility-mode
//! PSD carries exactly that picture in its image-data section — the same
//! flattened composite Photoshop shows for a quick look. Layers, blend modes
//! and adjustment layers are a compositor's job and deliberately not attempted.
//!
//! The section is simple enough to read by hand, which is what happens here
//! rather than reaching for a crate: a header, three skip-lengths, then the
//! samples planar (a channel at a time), either uncompressed or PackBits — and
//! the one existing Rust PSD crate had an uncompressed-path bug that silently
//! dropped the last channel, which is the kind of wrong colour nobody notices
//! until it is theirs.
//!
//! What is supported: RGB, greyscale and CMYK documents, with or without alpha,
//! at 8 and 16 bits per sample, raw or RLE — the shapes Photoshop itself writes.

/// What went wrong, in words a user could act on.
#[derive(Debug)]
pub struct PsdError(pub String);

impl std::fmt::Display for PsdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The flattened composite: interleaved RGBA8, straight (non-premultiplied)
/// alpha — the layout the rest of the app already speaks.
pub struct Composite {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// A cursor over big-endian bytes that answers with `None` at the end instead
/// of panicking.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.pos..self.pos + count)?;
        self.pos += count;
        Some(slice)
    }

    fn u16be(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32be(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    /// Skip a length-prefixed section, as the three between the header and the
    /// image data all are.
    fn skip_section(&mut self) -> Option<()> {
        let len = self.u32be()? as usize;
        self.take(len)?;
        Some(())
    }
}

/// Unwrap one PackBits-compressed row into `dest`, which is exactly one row of
/// one channel.
fn unpack_rle(row: &[u8], dest: &mut [u8]) -> Result<(), PsdError> {
    let mut src = 0;
    let mut out = 0;

    while out < dest.len() {
        let Some(&code) = row.get(src) else {
            return Err(PsdError("RLE row ended before the row was full".into()));
        };
        src += 1;

        match code {
            // Literal run: the next code+1 bytes are copied through.
            0..=127 => {
                let count = code as usize + 1;
                let literal = row
                    .get(src..src + count)
                    .ok_or_else(|| PsdError("RLE literal run past the end of its row".into()))?;
                dest.get_mut(out..out + count)
                    .ok_or_else(|| PsdError("RLE run overflows its row".into()))?
                    .copy_from_slice(literal);
                src += count;
                out += count;
            }
            // 128 is the no-op that keeps the codes signed.
            128 => {}
            // Repeated run: the next byte fills 257-code positions.
            129..=255 => {
                let count = 257 - code as usize;
                let value = *row
                    .get(src)
                    .ok_or_else(|| PsdError("RLE repeat run past the end of its row".into()))?;
                src += 1;
                dest.get_mut(out..out + count)
                    .ok_or_else(|| PsdError("RLE run overflows its row".into()))?
                    .fill(value);
                out += count;
            }
        }
    }

    Ok(())
}

/// Read the flattened composite out of a whole PSD file.
pub fn composite(data: &[u8]) -> Result<Composite, PsdError> {
    let short = |what: &str| PsdError(format!("the file ended inside the {what}"));

    let mut reader = Reader::new(data);

    // Header. Channels, not colour mode, decide how many planes follow: a
    // greyscale document has one or two, RGB three or four, CMYK four or five.
    if reader.take(4).ok_or_else(|| short("header"))? != b"8BPS" {
        return Err(PsdError("not a PSD file".into()));
    }
    let version = reader.u16be().ok_or_else(|| short("header"))?;
    if version != 1 {
        return Err(PsdError(format!(
            "PSB files (version {version}) are not supported"
        )));
    }
    reader.take(6).ok_or_else(|| short("header"))?;
    let channels = reader.u16be().ok_or_else(|| short("header"))? as usize;
    let height = reader.u32be().ok_or_else(|| short("header"))?;
    let width = reader.u32be().ok_or_else(|| short("header"))?;
    let depth = reader.u16be().ok_or_else(|| short("header"))?;
    let colour_mode = reader.u16be().ok_or_else(|| short("header"))?;

    if !matches!(channels, 1..=5) {
        return Err(PsdError(format!("unsupported channel count {channels}")));
    }
    if width == 0 || height == 0 {
        return Err(PsdError("the document has no pixels".into()));
    }
    if !matches!(depth, 8 | 16) {
        return Err(PsdError(format!("unsupported bit depth {depth}")));
    }
    if !matches!(colour_mode, 1 | 3 | 4) {
        return Err(PsdError(format!(
            "unsupported colour mode {colour_mode} (only greyscale, RGB and CMYK)"
        )));
    }

    // Colour mode data (the palette, for the modes we do not carry), image
    // resources, layer and mask information — all skipped whole.
    reader
        .skip_section()
        .ok_or_else(|| short("colour mode data"))?;
    reader
        .skip_section()
        .ok_or_else(|| short("image resources"))?;
    reader
        .skip_section()
        .ok_or_else(|| short("layer information"))?;

    let compression = reader.u16be().ok_or_else(|| short("compression marker"))?;
    if !matches!(compression, 0 | 1) {
        return Err(PsdError(format!("unsupported compression {compression}")));
    }

    let pixels_per_plane = width as usize * height as usize;
    let planes = channels * pixels_per_plane;

    // The samples, planar, straightened out of whichever encoding the file used.
    let mut samples = Vec::with_capacity(planes);
    match compression {
        0 => {
            let raw = reader
                .take(planes * if depth == 16 { 2 } else { 1 })
                .ok_or_else(|| short("image data"))?;
            if depth == 16 {
                // Big-endian 16-bit samples; the low byte is below the noise
                // floor of a viewer and is dropped rather than rounded.
                for sample in raw.chunks_exact(2) {
                    samples.push(sample[0]);
                }
            } else {
                samples.extend_from_slice(raw);
            }
        }
        1 => {
            // One byte-count per row per channel, then each channel's rows
            // compressed separately, in order: row r of channel c is the
            // (c * height + r)-th count, and the streams follow that same
            // order in the file. A 16-bit row decompresses to twice the
            // width; the high byte of each big-endian pair is the sample.
            let bytes_per_sample = usize::from(depth == 16) + 1;
            let row_len = width as usize * bytes_per_sample;
            let rows = channels * height as usize;
            let mut counts = Vec::with_capacity(rows);
            for _ in 0..rows {
                counts.push(reader.u16be().ok_or_else(|| short("RLE row table"))? as usize);
            }

            for channel in 0..channels {
                for r in 0..height as usize {
                    let count = counts[channel * height as usize + r];
                    let packed = reader.take(count).ok_or_else(|| short("RLE data"))?;
                    let mut dest = vec![0u8; row_len];
                    unpack_rle(packed, &mut dest)?;
                    if depth == 16 {
                        for pair in dest.chunks_exact(2) {
                            samples.push(pair[0]);
                        }
                    } else {
                        samples.extend_from_slice(&dest);
                    }
                }
            }
        }
        _ => unreachable!("checked above"),
    }

    if samples.len() < planes {
        return Err(PsdError("the image data ended early".into()));
    }
    samples.truncate(planes);

    // Planar to interleaved, and into the colour the app speaks.
    let mut pixels = vec![0u8; pixels_per_plane * 4];
    let sample = |pixel: usize, channel: usize| samples[channel * pixels_per_plane + pixel];

    for pixel in 0..pixels_per_plane {
        let rgba = &mut pixels[pixel * 4..pixel * 4 + 4];
        match (colour_mode, channels) {
            (3, 3) | (3, 4) => {
                rgba[0] = sample(pixel, 0);
                rgba[1] = sample(pixel, 1);
                rgba[2] = sample(pixel, 2);
                rgba[3] = if channels == 4 { sample(pixel, 3) } else { 255 };
            }
            (1, 1) | (1, 2) => {
                let luma = sample(pixel, 0);
                rgba[0] = luma;
                rgba[1] = luma;
                rgba[2] = luma;
                rgba[3] = if channels == 2 { sample(pixel, 1) } else { 255 };
            }
            (4, _) => {
                // CMYK stores ink coverage inverted — 0 means "full ink" — and
                // each output channel is 255 minus its ink minus the black.
                let ink = |c: usize| 255 - sample(pixel, c);
                let k = u16::from(ink(3));
                let channel = |c: usize| 255 - (u16::from(ink(c)) + k).min(255) as u8;
                rgba[0] = channel(0);
                rgba[1] = channel(1);
                rgba[2] = channel(2);
                rgba[3] = if channels == 5 { sample(pixel, 4) } else { 255 };
            }
            _ => {
                return Err(PsdError(format!(
                    "{channels} channels does not fit colour mode {colour_mode}"
                )));
            }
        }
    }

    Ok(Composite {
        width,
        height,
        pixels,
    })
}
