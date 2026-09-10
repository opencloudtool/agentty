use image::ImageFormat;

use crate::{ClipboardError, RgbaImageData};

pub(crate) fn decode_image_rgba(
    image_bytes: &[u8],
    image_format: ImageFormat,
) -> Result<RgbaImageData, ClipboardError> {
    let decoded_image = image::load_from_memory_with_format(image_bytes, image_format)
        .map_err(|error| {
            ClipboardError::image_conversion("failed to decode clipboard image", error)
        })?
        .into_rgba8();
    let (width, height) = decoded_image.dimensions();

    Ok(RgbaImageData {
        height,
        rgba_bytes: decoded_image.into_raw(),
        width,
    })
}

#[cfg(test)]
#[path = "format_test.rs"]
mod tests;
