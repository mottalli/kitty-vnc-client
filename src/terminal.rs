use std::io::{self, Stdout, Write};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::{cursor, execute, terminal};
use flate2::{Compression, write::ZlibEncoder};

use crate::Frame;

#[derive(Clone, Copy)]
pub(crate) struct Viewport {
    width: usize,
    height: usize,
    desktop_width: usize,
    desktop_height: usize,
    // If the PTY doesn't report pixel dimensions, place and track by cells.
    cells: Option<(usize, usize)>,
}

impl Viewport {
    fn fit(
        size: &terminal::WindowSize,
        desktop_width: usize,
        desktop_height: usize,
    ) -> Option<Self> {
        let cols = usize::from(size.columns);
        let rows = usize::from(size.rows);
        if cols == 0 || rows == 0 {
            return None;
        }
        let cell_width = usize::from(size.width) / cols;
        let cell_height = usize::from(size.height) / rows;
        let pixels = cell_width > 0 && cell_height > 0;
        let (cell_width, cell_height) = if pixels {
            (cell_width, cell_height)
        } else {
            (8, 16)
        };
        let (max_width, max_height) = (cols * cell_width, rows * cell_height);
        // Fit to the limiting axis, even when the desktop is smaller than the pane.
        let (width, height) = if max_width * desktop_height <= max_height * desktop_width {
            (
                max_width,
                (max_width * desktop_height / desktop_width).max(1),
            )
        } else {
            (
                (max_height * desktop_width / desktop_height).max(1),
                max_height,
            )
        };
        Some(Self {
            width,
            height,
            desktop_width,
            desktop_height,
            cells: (!pixels).then_some((width.div_ceil(cell_width), height.div_ceil(cell_height))),
        })
    }

    pub(crate) fn point(self, column: u16, row: u16, clamp: bool) -> Option<(u16, u16)> {
        let (width, height) = self.cells.unwrap_or((self.width, self.height));
        let (x, y) = (usize::from(column), usize::from(row));
        if !clamp && (x >= width || y >= height) {
            return None;
        }
        let scale = |value: usize, extent: usize, desktop: usize| {
            let value = value.min(extent - 1);
            // For cells, estimate the click at the cell center.
            let offset = if self.cells.is_some() { desktop / 2 } else { 0 };
            ((value * desktop + offset) / extent) as u16
        };
        Some((
            scale(x, width, self.desktop_width),
            scale(y, height, self.desktop_height),
        ))
    }
}

pub(crate) struct Terminal {
    out: Stdout,
    image_ids: [u32; 2],
    next: usize,
    mouse_mode: u16,
}

impl Terminal {
    pub(crate) fn open() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let id = (std::process::id() & 0x7fff_ffff).max(1) * 2;
        let mut screen = Self {
            out: io::stdout(),
            image_ids: [id, id + 1],
            next: 0,
            mouse_mode: 1006,
        };
        execute!(
            screen.out,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            cursor::MoveTo(0, 0)
        )?;
        write!(
            screen.out,
            "\x1b[?1003h\x1b[?1006hWaiting for VNC framebuffer (F12 quits)"
        )?;
        screen.out.flush()?;
        Ok(screen)
    }

    pub(crate) fn viewport(&self, width: usize, height: usize) -> io::Result<Option<Viewport>> {
        Ok(Viewport::fit(&terminal::window_size()?, width, height))
    }

    pub(crate) fn draw(&mut self, view: Viewport, frame: &Frame) -> io::Result<()> {
        let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
        compressor.write_all(&frame.scaled_rgb(view.width, view.height))?;
        let payload = STANDARD.encode(compressor.finish()?);
        let id = self.image_ids[self.next];
        let old = self.image_ids[1 - self.next];

        let mut out = io::BufWriter::new(self.out.lock());
        let chunks = payload.as_bytes().chunks(4096);
        let count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            let more = u8::from(index + 1 < count);
            if index == 0 {
                write!(
                    out,
                    "\x1b_Ga=t,f=24,o=z,s={},v={},i={id},q=2,m={more};",
                    view.width, view.height
                )?;
            } else {
                write!(out, "\x1b_Gm={more};")?;
            }
            out.write_all(chunk)?;
            out.write_all(b"\x1b\\")?;
        }

        // Upload out of sight, then swap the placements at the pane origin.
        write!(out, "\x1b[?2026h\x1b[H\x1b[2K\x1b_Ga=p,i={id},C=1,q=2")?;
        if let Some((cols, rows)) = view.cells {
            write!(out, ",c={cols},r={rows}")?;
        }
        write!(out, ";\x1b\\\x1b_Ga=d,d=I,i={old},q=2;\x1b\\\x1b[?2026l")?;
        let mode = if view.cells.is_some() { 1006 } else { 1016 };
        if mode != self.mouse_mode {
            write!(out, "\x1b[?{}l\x1b[?{mode}h", self.mouse_mode)?;
            self.mouse_mode = mode;
        }
        out.flush()?;
        self.next = 1 - self.next;
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // ST also terminates a graphics command interrupted by an I/O error.
        let _ = write!(
            self.out,
            "\x1b\\\x1b[?2026l\x1b[?1016l\x1b[?1006l\x1b[?1003l"
        );
        for id in self.image_ids {
            let _ = write!(self.out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\");
        }
        let _ = execute!(self.out, cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_pane_and_maps_pixels() {
        let size = terminal::WindowSize {
            columns: 80,
            rows: 24,
            width: 800,
            height: 480,
        };
        let view = Viewport::fit(&size, 1600, 900).unwrap();
        assert_eq!((view.width, view.height), (800, 450));
        assert_eq!(view.point(400, 225, false), Some((800, 450)));
        assert_eq!(view.point(400, 451, false), None);
        assert_eq!(view.point(400, 451, true), Some((800, 898)));
    }

    #[test]
    fn scales_up_a_smaller_desktop_and_maps_clicks_in_the_added_area() {
        let size = terminal::WindowSize {
            columns: 100,
            rows: 60,
            width: 1600,
            height: 1320,
        };
        let view = Viewport::fit(&size, 1280, 1024).unwrap();
        assert_eq!((view.width, view.height), (1600, 1280));
        assert_eq!(view.point(1500, 1200, false), Some((1200, 960)));
        assert_eq!(view.point(1500, 1280, false), None);

        let portrait = Viewport::fit(&size, 600, 800).unwrap();
        assert_eq!((portrait.width, portrait.height), (990, 1320));
        assert_eq!(portrait.point(900, 1200, false), Some((545, 727)));
        assert_eq!(portrait.point(990, 1200, false), None);
    }

    #[test]
    fn falls_back_to_cell_coordinates_without_pixel_dimensions() {
        let size = terminal::WindowSize {
            columns: 80,
            rows: 24,
            width: 0,
            height: 0,
        };
        let view = Viewport::fit(&size, 320, 192).unwrap();
        assert_eq!((view.width, view.height), (640, 384));
        assert_eq!(view.cells, Some((80, 24)));
        assert_eq!(view.point(0, 0, false), Some((2, 4)));
        assert_eq!(view.point(80, 0, false), None);
    }
}
