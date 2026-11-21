pub mod backend;
mod cli;
mod input;
mod session;
mod terminal;
mod viewer;

use std::io;

use anyhow::{Result, ensure};
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    images::{Image, ImageRef},
};
use terminal::{RenderOptions, Sampling};

pub use cli::{ConnectionTarget, ViewerArgs, parse_viewer_args};
pub use session::require_terminal;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum RenderMode {
    #[default]
    Kitty,
    Text,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewerOptions {
    pub(crate) render: RenderMode,
    pub(crate) initial: RenderOptions,
}

pub struct Frame {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(crate) struct FrameRegion {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Frame {
    pub fn new(width: u16, height: u16) -> Self {
        let (width, height) = (usize::from(width), usize::from(height));
        Self {
            width,
            height,
            rgba: vec![0; width * height * 4],
        }
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.rgba
    }

    pub(crate) fn update(&mut self, rect: FrameRegion, pixels: &[u8]) -> Result<()> {
        let [x, y, width, height] = [rect.x, rect.y, rect.width, rect.height].map(usize::from);
        ensure!(
            x + width <= self.width
                && y + height <= self.height
                && pixels.len() == width * height * 4,
            "invalid framebuffer rectangle"
        );
        for row in 0..height {
            let src = row * width * 4;
            let dst = ((y + row) * self.width + x) * 4;
            self.rgba[dst..dst + width * 4].copy_from_slice(&pixels[src..src + width * 4]);
        }
        Ok(())
    }

    fn scaled_rgb(&self, width: usize, height: usize, sampling: Sampling) -> io::Result<Vec<u8>> {
        let rgb_pixels = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect()
        };
        if (width, height) == (self.width, self.height) {
            return Ok(rgb_pixels(&self.rgba));
        }
        if sampling == Sampling::Smooth {
            let src = ImageRef::new(
                self.width as u32,
                self.height as u32,
                &self.rgba,
                PixelType::U8x4,
            )
            .map_err(io::Error::other)?;
            let mut dst = Image::new(width as u32, height as u32, PixelType::U8x4);
            // X11/VNC pixels have a padding byte, not alpha.
            let options = ResizeOptions::new()
                .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                .use_alpha(false);
            Resizer::new()
                .resize(&src, &mut dst, &options)
                .map_err(io::Error::other)?;
            return Ok(rgb_pixels(dst.buffer()));
        }

        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            let row = (y * self.height / height) * self.width;
            for x in 0..width {
                let offset = (row + x * self.width / width) * 4;
                rgb.extend_from_slice(&self.rgba[offset..offset + 3]);
            }
        }
        Ok(rgb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_rectangles_and_ignores_pixel_padding() {
        let mut frame = Frame::new(2, 2);
        let rect = FrameRegion {
            x: 1,
            y: 0,
            width: 1,
            height: 2,
        };
        frame.update(rect, &[1, 2, 3, 0, 4, 5, 6, 255]).unwrap();
        for sampling in [Sampling::Smooth, Sampling::Nearest] {
            assert_eq!(
                frame.scaled_rgb(2, 2, sampling).unwrap(),
                [0, 0, 0, 1, 2, 3, 0, 0, 0, 4, 5, 6]
            );
        }
        assert!(frame.update(rect, &[1, 2, 3]).is_err());
    }

    #[test]
    fn smoothing_blends_edges_when_enlarging_or_shrinking() {
        let mut frame = Frame::new(2, 1);
        frame
            .pixels_mut()
            .copy_from_slice(&[0, 0, 0, 255, 255, 255, 255, 0]);
        for (width, nearest) in [
            (3, vec![0, 0, 0, 0, 0, 0, 255, 255, 255]),
            (1, vec![0, 0, 0]),
        ] {
            let smooth = frame.scaled_rgb(width, 1, Sampling::Smooth).unwrap();
            assert_eq!(
                frame.scaled_rgb(width, 1, Sampling::Nearest).unwrap(),
                nearest
            );
            let middle = (width / 2) * 3;
            assert_eq!(&smooth[middle..middle + 3], &[128; 3]);
        }
    }
}
