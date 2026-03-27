//! Screenshot geometry calibration for mapping terminal cells to pixels.
//!
//! Derives a cell-to-pixel transform from VHS font, padding, and viewport
//! settings so that terminal cell coordinates can be accurately mapped onto
//! screenshot pixel coordinates for overlays and visual failure reporting.

use crate::region::Region;

/// Cell-to-pixel transform derived from VHS capture settings.
///
/// Stores the pixel origin and per-cell dimensions so any terminal
/// `(col, row)` can be mapped to a screenshot `(x, y, width, height)`
/// rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// X offset in pixels from the screenshot left edge to the first cell.
    pub origin_x: f64,
    /// Y offset in pixels from the screenshot top edge to the first cell.
    pub origin_y: f64,
    /// Width of a single cell in pixels.
    pub cell_width: f64,
    /// Height of a single cell in pixels.
    pub cell_height: f64,
}

/// A pixel-space rectangle on a screenshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRect {
    /// X coordinate of the top-left corner.
    pub x_coordinate: f64,
    /// Y coordinate of the top-left corner.
    pub y_coordinate: f64,
    /// Width in pixels.
    pub width: f64,
    /// Height in pixels.
    pub height: f64,
}

impl Calibration {
    /// Create a calibration from explicit measurements.
    pub fn new(origin_x: f64, origin_y: f64, cell_width: f64, cell_height: f64) -> Self {
        Self {
            origin_x,
            origin_y,
            cell_width,
            cell_height,
        }
    }

    /// Derive calibration from screenshot dimensions and terminal grid size.
    ///
    /// Assumes zero padding and evenly distributed cells across the image.
    pub fn from_image_size(image_width: u32, image_height: u32, cols: u16, rows: u16) -> Self {
        let cell_width = f64::from(image_width) / f64::from(cols);
        let cell_height = f64::from(image_height) / f64::from(rows);

        Self {
            origin_x: 0.0,
            origin_y: 0.0,
            cell_width,
            cell_height,
        }
    }

    /// Convert a terminal [`Region`] to a [`PixelRect`] on the screenshot.
    pub fn region_to_pixels(&self, region: &Region) -> PixelRect {
        PixelRect {
            x_coordinate: self.origin_x + f64::from(region.col) * self.cell_width,
            y_coordinate: self.origin_y + f64::from(region.row) * self.cell_height,
            width: f64::from(region.width) * self.cell_width,
            height: f64::from(region.height) * self.cell_height,
        }
    }

    /// Convert a pixel coordinate back to the nearest terminal cell.
    pub fn pixel_to_cell(&self, pixel_x: f64, pixel_y: f64) -> (u16, u16) {
        let col = safe_f64_to_u16(((pixel_x - self.origin_x) / self.cell_width).floor());
        let row = safe_f64_to_u16(((pixel_y - self.origin_y) / self.cell_height).floor());

        (col, row)
    }
}

impl PixelRect {
    /// Return the right edge X coordinate.
    pub fn right(&self) -> f64 {
        self.x_coordinate + self.width
    }

    /// Return the bottom edge Y coordinate.
    pub fn bottom(&self) -> f64 {
        self.y_coordinate + self.height
    }

    /// Convert to integer bounds for image operations (x, y, width, height).
    pub fn to_integer_bounds(&self) -> (u32, u32, u32, u32) {
        (
            safe_f64_to_u32(self.x_coordinate.round()),
            safe_f64_to_u32(self.y_coordinate.round()),
            safe_f64_to_u32(self.width.round()),
            safe_f64_to_u32(self.height.round()),
        )
    }
}

/// Safely convert a non-negative `f64` to `u16`, clamping to `[0, u16::MAX]`.
///
/// Negative and `NaN` values map to `0`; values above `u16::MAX` saturate.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0.0, 65535.0] before the cast"
)]
fn safe_f64_to_u16(value: f64) -> u16 {
    value.clamp(0.0, f64::from(u16::MAX)) as u16
}

/// Safely convert a non-negative `f64` to `u32`, clamping to `[0, u32::MAX]`.
///
/// Negative and `NaN` values map to `0`; values above `u32::MAX` saturate.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0.0, u32::MAX as f64] before the cast"
)]
fn safe_f64_to_u32(value: f64) -> u32 {
    value.clamp(0.0, f64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_image_size_computes_cell_dimensions() {
        // Arrange / Act
        let calibration = Calibration::from_image_size(800, 480, 80, 24);

        // Assert
        assert!((calibration.cell_width - 10.0).abs() < f64::EPSILON);
        assert!((calibration.cell_height - 20.0).abs() < f64::EPSILON);
        assert!((calibration.origin_x).abs() < f64::EPSILON);
    }

    #[test]
    fn region_to_pixels_maps_correctly() {
        // Arrange
        let calibration = Calibration::new(0.0, 0.0, 10.0, 20.0);
        let region = Region::new(5, 2, 10, 3);

        // Act
        let pixel_rect = calibration.region_to_pixels(&region);

        // Assert
        assert!((pixel_rect.x_coordinate - 50.0).abs() < f64::EPSILON);
        assert!((pixel_rect.y_coordinate - 40.0).abs() < f64::EPSILON);
        assert!((pixel_rect.width - 100.0).abs() < f64::EPSILON);
        assert!((pixel_rect.height - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn region_to_pixels_with_origin_offset() {
        // Arrange
        let calibration = Calibration::new(10.0, 5.0, 8.0, 16.0);
        let region = Region::new(0, 0, 1, 1);

        // Act
        let pixel_rect = calibration.region_to_pixels(&region);

        // Assert
        assert!((pixel_rect.x_coordinate - 10.0).abs() < f64::EPSILON);
        assert!((pixel_rect.y_coordinate - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pixel_to_cell_round_trips() {
        // Arrange
        let calibration = Calibration::new(0.0, 0.0, 10.0, 20.0);

        // Act
        let (col, row) = calibration.pixel_to_cell(55.0, 45.0);

        // Assert
        assert_eq!(col, 5);
        assert_eq!(row, 2);
    }

    #[test]
    fn pixel_rect_to_integer_bounds() {
        // Arrange
        let rect = PixelRect {
            x_coordinate: 10.4,
            y_coordinate: 20.6,
            width: 100.3,
            height: 50.7,
        };

        // Act
        let (x_bound, y_bound, width_bound, height_bound) = rect.to_integer_bounds();

        // Assert
        assert_eq!(x_bound, 10);
        assert_eq!(y_bound, 21);
        assert_eq!(width_bound, 100);
        assert_eq!(height_bound, 51);
    }
}
