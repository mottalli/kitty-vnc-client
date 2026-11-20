mod input;
mod terminal;

use std::io;

use anyhow::{Result, ensure};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    images::{Image, ImageRef},
};
use terminal::{Control, Pane, RenderOptions, Sampling, Terminal, Viewport};

pub use input::{KEY_MODIFIERS, key_tap};

pub struct Frame {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
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

    pub fn update(&mut self, rect: (u16, u16, u16, u16), pixels: &[u8]) -> Result<()> {
        let [x, y, width, height] = [rect.0, rect.1, rect.2, rect.3].map(usize::from);
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

#[derive(Clone, Copy)]
pub struct PointerUpdate {
    pub x: u16,
    pub y: u16,
    /// X button 1 is bit 0, button 2 is bit 1, and so on.
    pub buttons: u8,
    pub wheel: u8,
}

pub enum ViewerEvent {
    None,
    Quit,
    Key(KeyEvent),
    Pointer(PointerUpdate),
}

pub struct Viewer {
    terminal: Terminal,
    pane: Pane,
    options: RenderOptions,
    viewport: Option<Viewport>,
    pointer: input::Pointer,
    pressed: Option<Control>,
    dirty: bool,
}

impl Viewer {
    pub fn open() -> io::Result<Self> {
        let mut terminal = Terminal::open()?;
        let pane = terminal.pane()?;
        let options = RenderOptions::default();
        let viewport = terminal.draw(pane, None, options)?;
        Ok(Self {
            terminal,
            pane,
            options,
            viewport,
            pointer: input::Pointer::default(),
            pressed: None,
            dirty: false,
        })
    }

    pub fn render(
        &mut self,
        frame: Option<&Frame>,
        changed: bool,
    ) -> io::Result<Option<PointerUpdate>> {
        // Pixel dimensions can change without the row or column counts changing.
        let pane = self.terminal.pane()?;
        if changed || self.dirty || self.pane != pane {
            self.viewport = self.terminal.draw(pane, frame, self.options)?;
            self.pane = pane;
            self.dirty = false;
            if self.viewport.is_none() {
                return Ok(self.pointer.release());
            }
        }
        Ok(None)
    }

    pub fn reset_pointer(&mut self) -> Option<PointerUpdate> {
        self.viewport = None;
        self.pointer.release()
    }

    pub fn handle(&mut self, event: Event) -> ViewerEvent {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::F(12) {
                    ViewerEvent::Quit
                } else {
                    ViewerEvent::Key(key)
                }
            }
            Event::Mouse(mouse) => {
                let hit = self.pane.control_at(mouse.column, mouse.row, self.options);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) if hit.is_some() => {
                        self.pressed = hit;
                    }
                    MouseEventKind::Drag(MouseButton::Left) if self.pressed.is_some() => {}
                    MouseEventKind::Up(MouseButton::Left) if self.pressed.is_some() => {
                        if self.pressed.take() == hit {
                            match hit {
                                Some(Control::Close) => return ViewerEvent::Quit,
                                Some(Control::Scale) => {
                                    self.options.scale = self.options.scale.toggled();
                                    self.dirty = true;
                                }
                                Some(Control::Smoothing) => {
                                    self.options.sampling = self.options.sampling.toggled();
                                    self.dirty = true;
                                }
                                None => {}
                            }
                        }
                    }
                    _ => {
                        if let Some(update) = self
                            .viewport
                            .and_then(|view| self.pointer.update(mouse, view))
                        {
                            return ViewerEvent::Pointer(update);
                        }
                    }
                }
                ViewerEvent::None
            }
            Event::Resize(..) => {
                self.dirty = true;
                ViewerEvent::None
            }
            _ => ViewerEvent::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_rectangles_and_ignores_pixel_padding() {
        let mut frame = Frame::new(2, 2);
        let rect = (1, 0, 1, 2);
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
