use std::io::{self, Stdout, Write};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::{cursor, execute, terminal};
use flate2::{Compression, write::ZlibEncoder};

use crate::Frame;

const CLOSE_LABEL: &str = "[ Close ]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScaleMode {
    Fit,
    Fill,
}

impl ScaleMode {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Fit => Self::Fill,
            Self::Fill => Self::Fit,
        }
    }

    fn label(self, compact: bool) -> &'static str {
        match (self, compact) {
            (Self::Fit, false) => "[ Fit  ]",
            (Self::Fill, false) => "[ Fill ]",
            (Self::Fit, true) => "F",
            (Self::Fill, true) => "S",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    Scale,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pane {
    cols: usize,
    rows: usize,
    cell_width: usize,
    cell_height: usize,
    pixels: bool,
}

impl Pane {
    fn new(size: terminal::WindowSize) -> Self {
        let (cols, rows) = (usize::from(size.columns), usize::from(size.rows));
        let cell_width = usize::from(size.width) / cols.max(1);
        let cell_height = usize::from(size.height) / rows.max(1);
        let pixels = cell_width > 0 && cell_height > 0;
        let (cell_width, cell_height) = if pixels {
            (cell_width, cell_height)
        } else {
            (8, 16)
        };
        Self {
            cols,
            rows,
            cell_width,
            cell_height,
            pixels,
        }
    }

    fn buttons(self, scale: ScaleMode) -> impl Iterator<Item = (usize, &'static str, Control)> {
        let close = match self.cols {
            0 => None,
            1 | 2 => Some((self.cols - 1, "x", Control::Close)),
            cols if cols >= CLOSE_LABEL.len() + 4 => {
                Some((cols - 1 - CLOSE_LABEL.len(), CLOSE_LABEL, Control::Close))
            }
            cols => Some((cols - 2, "x", Control::Close)),
        };
        let scale_toggle = close.and_then(|(col, _, _)| {
            let label = scale.label(false);
            if col > label.len() + 1 {
                Some((col - label.len() - 1, label, Control::Scale))
            } else if self.cols >= 4 {
                Some((
                    col.saturating_sub(2).max(1),
                    scale.label(true),
                    Control::Scale,
                ))
            } else {
                None
            }
        });
        scale_toggle.into_iter().chain(close)
    }

    pub(crate) fn control_at(self, column: u16, row: u16, scale: ScaleMode) -> Option<Control> {
        let (x, y) = (usize::from(column), usize::from(row));
        let (col, row) = if self.pixels {
            (x / self.cell_width, y / self.cell_height)
        } else {
            (x, y)
        };
        if row != 0 || self.rows == 0 {
            return None;
        }
        self.buttons(scale)
            .find(|(start, label, _)| (*start..*start + label.len()).contains(&col))
            .map(|(_, _, control)| control)
    }

    fn paint(self, mut out: impl Write, scale: ScaleMode) -> io::Result<()> {
        write!(out, "\x1b[2J")?;
        if self.cols == 0 || self.rows == 0 {
            return Ok(());
        }
        let mut buttons = self.buttons(scale).peekable();
        write!(out, "\x1b[H")?;
        let mut col = 0;
        if self.cols >= 3 {
            write!(out, "╭")?;
            col = 1;
            if buttons.peek().is_some_and(|(start, _, _)| *start >= 7) {
                write!(out, " VNC ")?;
                col += " VNC ".len();
            }
        }
        let divider = if self.cols >= 3 { "─" } else { " " };
        for (start, label, _) in buttons {
            write!(out, "{}\x1b[7m{label}\x1b[27m", divider.repeat(start - col))?;
            col = start + label.len();
        }
        if self.cols >= 3 {
            write!(out, "╮")?;
            if self.rows >= 2 {
                for row in 2..self.rows {
                    write!(out, "\x1b[{row};1H│\x1b[{row};{}H│", self.cols)?;
                }
                write!(out, "\x1b[{};1H╰{}╯", self.rows, "─".repeat(self.cols - 2))?;
            }
        }
        Ok(())
    }

    fn paint_scale(self, mut out: impl Write, scale: ScaleMode) -> io::Result<()> {
        if self.rows > 0
            && let Some((col, label, _)) =
                self.buttons(scale).find(|entry| entry.2 == Control::Scale)
        {
            write!(out, "\x1b[1;{}H\x1b[7m{label}\x1b[27m", col + 1)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Viewport {
    // Image origin in the same units as the terminal's mouse reports.
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    desktop_width: usize,
    desktop_height: usize,
    // If the PTY doesn't report pixel dimensions, place and track by cells.
    cells: Option<(usize, usize)>,
}

impl Viewport {
    fn new(
        pane: Pane,
        desktop_width: usize,
        desktop_height: usize,
        scale: ScaleMode,
    ) -> Option<Self> {
        if pane.cols < 3 || pane.rows < 3 {
            return None;
        }
        let cols = pane.cols - 2;
        let rows = pane.rows - 2;
        let (max_width, max_height) = (cols * pane.cell_width, rows * pane.cell_height);
        let (width, height) = match scale {
            ScaleMode::Fill => (max_width, max_height),
            ScaleMode::Fit if max_width * desktop_height <= max_height * desktop_width => (
                max_width,
                (max_width * desktop_height / desktop_width).max(1),
            ),
            ScaleMode::Fit => (
                (max_height * desktop_width / desktop_height).max(1),
                max_height,
            ),
        };
        let (left, top, cells) = if pane.pixels {
            (
                pane.cell_width + (max_width - width) / 2,
                pane.cell_height + (max_height - height) / 2,
                None,
            )
        } else {
            let image_cols = width.div_ceil(pane.cell_width);
            let image_rows = height.div_ceil(pane.cell_height);
            (
                1 + (cols - image_cols) / 2,
                1 + (rows - image_rows) / 2,
                Some((image_cols, image_rows)),
            )
        };
        Some(Self {
            left,
            top,
            width,
            height,
            desktop_width,
            desktop_height,
            cells,
        })
    }

    pub(crate) fn point(self, column: u16, row: u16, clamp: bool) -> Option<(u16, u16)> {
        let (width, height) = self.cells.unwrap_or((self.width, self.height));
        let (x, y) = (usize::from(column), usize::from(row));
        if !clamp
            && (x < self.left || y < self.top || x >= self.left + width || y >= self.top + height)
        {
            return None;
        }
        let scale = |value: usize, extent: usize, desktop: usize| {
            let value = value.min(extent - 1);
            // For cells, estimate the click at the cell center.
            let offset = if self.cells.is_some() { desktop / 2 } else { 0 };
            ((value * desktop + offset) / extent) as u16
        };
        Some((
            scale(x.saturating_sub(self.left), width, self.desktop_width),
            scale(y.saturating_sub(self.top), height, self.desktop_height),
        ))
    }

    fn place(self, mut out: impl Write, pane: Pane, id: u32) -> io::Result<()> {
        let (col, row) = if pane.pixels {
            (self.left / pane.cell_width, self.top / pane.cell_height)
        } else {
            (self.left, self.top)
        };
        write!(out, "\x1b[{};{}H\x1b_Ga=p,i={id},C=1,q=2", row + 1, col + 1)?;
        if let Some((cols, rows)) = self.cells {
            write!(out, ",c={cols},r={rows}")?;
        } else {
            write!(
                out,
                ",X={},Y={}",
                self.left % pane.cell_width,
                self.top % pane.cell_height
            )?;
        }
        write!(out, ";\x1b\\")
    }
}

pub(crate) struct Terminal {
    out: Stdout,
    image_ids: [u32; 2],
    next: usize,
    mouse_mode: u16,
    painted: Option<(Pane, ScaleMode)>,
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
            painted: None,
        };
        execute!(
            screen.out,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            cursor::MoveTo(0, 0)
        )?;
        write!(screen.out, "\x1b[?1003h\x1b[?1006h")?;
        screen.out.flush()?;
        Ok(screen)
    }

    pub(crate) fn pane(&self) -> io::Result<Pane> {
        terminal::window_size().map(Pane::new)
    }

    pub(crate) fn draw(
        &mut self,
        pane: Pane,
        frame: Option<&Frame>,
        scale: ScaleMode,
    ) -> io::Result<Option<Viewport>> {
        let image = frame.and_then(|frame| {
            Viewport::new(pane, frame.width, frame.height, scale).map(|view| (view, frame))
        });
        let mut out = io::BufWriter::new(self.out.lock());
        draw_frame(
            &mut out,
            pane,
            scale,
            self.painted,
            image,
            [self.image_ids[self.next], self.image_ids[1 - self.next]],
        )?;
        self.painted = Some((pane, scale));
        if image.is_some() {
            self.next = 1 - self.next;
        }
        let mode = if pane.pixels { 1016 } else { 1006 };
        if mode != self.mouse_mode {
            write!(out, "\x1b[?{}l\x1b[?{mode}h", self.mouse_mode)?;
            self.mouse_mode = mode;
        }
        out.flush()?;
        Ok(image.map(|(view, _)| view))
    }
}

fn draw_frame(
    mut out: impl Write,
    pane: Pane,
    scale: ScaleMode,
    previous: Option<(Pane, ScaleMode)>,
    image: Option<(Viewport, &Frame)>,
    [id, old]: [u32; 2],
) -> io::Result<()> {
    let repaint = previous.is_none_or(|(prev, _)| prev != pane);
    if repaint {
        // Some terminals discard Kitty images on a screen clear, including uploads
        // that haven't been placed yet. Clear and repaint before uploading.
        pane.paint(&mut out, scale)?;
    }
    if let Some((view, frame)) = image {
        upload(&mut out, id, view, frame)?;
    }

    // Swap the placement and update the label without clearing the screen.
    write!(out, "\x1b[?2026h")?;
    if !repaint && previous != Some((pane, scale)) {
        pane.paint_scale(&mut out, scale)?;
    }
    if let Some((view, _)) = image {
        view.place(&mut out, pane, id)?;
    }
    write!(out, "\x1b_Ga=d,d=I,i={old},q=2;\x1b\\\x1b[?2026l")
}

fn upload(mut out: impl Write, id: u32, view: Viewport, frame: &Frame) -> io::Result<()> {
    let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
    compressor.write_all(&frame.scaled_rgb(view.width, view.height))?;
    let payload = STANDARD.encode(compressor.finish()?);
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
    Ok(())
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
    fn scale_toggle_replaces_image_without_clearing_the_screen() {
        let pane = Pane::new(terminal::WindowSize {
            columns: 40,
            rows: 20,
            width: 320,
            height: 320,
        });
        let frame = Frame::new(16, 10);
        for old_scale in [ScaleMode::Fit, ScaleMode::Fill] {
            let scale = old_scale.toggled();
            let view = Viewport::new(pane, frame.width, frame.height, scale).unwrap();
            let mut output = Vec::new();
            draw_frame(
                &mut output,
                pane,
                scale,
                Some((pane, old_scale)),
                Some((view, &frame)),
                [2, 1],
            )
            .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(!output.contains("\x1b[2J"));
            assert!(output.contains(scale.label(false)));
            let upload = output.find("\x1b_Ga=t,").unwrap();
            let place = output.find("\x1b_Ga=p,i=2,").unwrap();
            let delete = output.find("\x1b_Ga=d,d=I,i=1,").unwrap();
            assert!(upload < place && place < delete);
        }
    }

    #[test]
    fn resize_clears_before_uploading_the_replacement_image() {
        let old_pane = Pane::new(terminal::WindowSize {
            columns: 40,
            rows: 20,
            width: 320,
            height: 320,
        });
        let pane = Pane {
            cols: 30,
            ..old_pane
        };
        let frame = Frame::new(16, 10);
        let scale = ScaleMode::Fit;
        let view = Viewport::new(pane, frame.width, frame.height, scale).unwrap();
        let mut output = Vec::new();
        draw_frame(
            &mut output,
            pane,
            scale,
            Some((old_pane, scale)),
            Some((view, &frame)),
            [2, 1],
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        let clear = output.rfind("\x1b[2J").unwrap();
        let upload = output.find("\x1b_Ga=t,").unwrap();
        let place = output.find("\x1b_Ga=p,i=2,").unwrap();
        assert!(clear < upload && upload < place);
    }

    #[test]
    fn fits_pane_and_maps_pixels() {
        let pane = Pane::new(terminal::WindowSize {
            columns: 80,
            rows: 24,
            width: 800,
            height: 480,
        });
        let view = Viewport::new(pane, 1600, 900, ScaleMode::Fit).unwrap();
        assert_eq!(
            (view.left, view.top, view.width, view.height),
            (10, 21, 780, 438)
        );
        assert_eq!(view.point(400, 240, false), Some((800, 450)));
        assert_eq!(view.point(400, 20, false), None);
        assert_eq!(view.point(9, 240, false), None);
        assert_eq!(view.point(400, 459, false), None);
        assert_eq!(view.point(400, 459, true), Some((800, 897)));
        assert_eq!(view.point(0, 0, true), Some((0, 0)));

        let mut placement = Vec::new();
        view.place(&mut placement, pane, 7).unwrap();
        assert_eq!(placement, b"\x1b[2;2H\x1b_Ga=p,i=7,C=1,q=2,X=0,Y=1;\x1b\\");
    }

    #[test]
    fn scales_up_a_smaller_desktop_and_maps_clicks_in_the_added_area() {
        let pane = Pane::new(terminal::WindowSize {
            columns: 100,
            rows: 60,
            width: 1600,
            height: 1320,
        });
        let view = Viewport::new(pane, 1280, 1024, ScaleMode::Fit).unwrap();
        assert_eq!(
            (view.left, view.top, view.width, view.height),
            (16, 33, 1568, 1254)
        );
        assert_eq!(view.point(800, 660, false), Some((640, 512)));
        assert!(view.point(1500, 1200, false).is_some());
        assert_eq!(view.point(1500, 1287, false), None);

        let portrait = Viewport::new(pane, 600, 800, ScaleMode::Fit).unwrap();
        assert_eq!(
            (portrait.left, portrait.top, portrait.width, portrait.height),
            (321, 22, 957, 1276)
        );
        assert_eq!(portrait.point(1120, 660, false), Some((500, 400)));
        assert_eq!(portrait.point(1278, 660, false), None);
    }

    #[test]
    fn falls_back_to_cell_coordinates_without_pixel_dimensions() {
        let pane = Pane::new(terminal::WindowSize {
            columns: 80,
            rows: 24,
            width: 0,
            height: 0,
        });
        let view = Viewport::new(pane, 320, 192, ScaleMode::Fit).unwrap();
        assert_eq!(
            (view.left, view.top, view.width, view.height),
            (3, 1, 586, 352)
        );
        assert_eq!(view.cells, Some((74, 22)));
        assert_eq!(view.point(3, 1, false), Some((2, 4)));
        assert_eq!(view.point(2, 1, false), None);
        assert_eq!(pane.control_at(70, 0, ScaleMode::Fit), Some(Control::Close));
        assert_eq!(pane.control_at(70, 1, ScaleMode::Fit), None);

        let fill = Viewport::new(pane, 320, 192, ScaleMode::Fill).unwrap();
        assert_eq!((fill.left, fill.top, fill.cells), (1, 1, Some((78, 22))));
    }

    #[test]
    fn fill_covers_the_interior_and_maps_each_axis_independently() {
        let pane = Pane::new(terminal::WindowSize {
            columns: 80,
            rows: 24,
            width: 800,
            height: 480,
        });
        let scale = ScaleMode::Fit.toggled();
        assert_eq!(scale.toggled(), ScaleMode::Fit);
        let view = Viewport::new(pane, 320, 100, scale).unwrap();
        assert_eq!(
            (view.left, view.top, view.width, view.height),
            (10, 20, 780, 440)
        );
        assert_eq!(view.point(10, 20, false), Some((0, 0)));
        assert_eq!(view.point(400, 240, false), Some((160, 50)));
        assert_eq!(view.point(789, 459, false), Some((319, 99)));
        assert_eq!(view.point(789, 460, false), None);
    }

    #[test]
    fn toolbar_hitboxes_track_the_painted_buttons_in_pixels() {
        let pane = Pane::new(terminal::WindowSize {
            columns: 80,
            rows: 24,
            width: 800,
            height: 480,
        });
        let mut border = Vec::new();
        pane.paint(&mut border, ScaleMode::Fill).unwrap();
        assert!(
            String::from_utf8(border)
                .unwrap()
                .contains("\x1b[7m[ Fill ]\x1b[27m─\x1b[7m[ Close ]\x1b[27m╮")
        );
        for scale in [ScaleMode::Fit, ScaleMode::Fill] {
            for (x, y, hit) in [
                (609, 19, None),
                (610, 19, Some(Control::Scale)),
                (689, 19, Some(Control::Scale)),
                (699, 19, None),
                (700, 19, Some(Control::Close)),
                (789, 19, Some(Control::Close)),
                (790, 19, None),
                (700, 20, None),
            ] {
                assert_eq!(pane.control_at(x, y, scale), hit);
            }
        }
    }

    #[test]
    fn a_tiny_pane_keeps_a_close_button_without_a_viewport() {
        for cols in 1..32 {
            let pane = Pane::new(terminal::WindowSize {
                columns: cols,
                rows: 2,
                width: 0,
                height: 0,
            });
            pane.paint(Vec::new(), ScaleMode::Fit).unwrap();
            let hits: Vec<_> = (0..cols)
                .filter_map(|col| pane.control_at(col, 0, ScaleMode::Fit))
                .collect();
            assert!(hits.contains(&Control::Close));
            assert_eq!(hits.contains(&Control::Scale), cols >= 4);
            assert!(Viewport::new(pane, 320, 192, ScaleMode::Fit).is_none());
        }
    }
}
