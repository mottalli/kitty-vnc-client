mod geometry;
mod kitty;
mod text;

use std::io::{self, Stdout, Write};

use crossterm::{cursor, execute, terminal};

use crate::{Frame, RenderMode};

use geometry::MouseCoordinates;
pub(crate) use geometry::{Pane, Viewport};
use kitty::KittyRenderer;
use text::TextRenderer;

const CLOSE_LABEL: &str = "[ Close ]";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ScaleMode {
    #[default]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Sampling {
    #[default]
    Smooth,
    Nearest,
}

impl Sampling {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Smooth => Self::Nearest,
            Self::Nearest => Self::Smooth,
        }
    }

    fn label(self, compact: bool) -> &'static str {
        match (self, compact) {
            (Self::Smooth, false) => "[ Smooth: on  ]",
            (Self::Nearest, false) => "[ Smooth: off ]",
            (Self::Smooth, true) => "[S+]",
            (Self::Nearest, true) => "[S-]",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RenderOptions {
    pub(crate) scale: ScaleMode,
    pub(crate) sampling: Sampling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    Scale,
    Smoothing,
    Close,
}

struct Button {
    col: usize,
    label: &'static str,
    control: Control,
}

impl Pane {
    fn buttons(self, options: RenderOptions) -> impl Iterator<Item = Button> {
        let close = (self.cols > 0).then(|| {
            let (col, label) = match self.cols {
                1 | 2 => (self.cols - 1, "x"),
                cols if cols >= CLOSE_LABEL.len() + 4 => {
                    (cols - 1 - CLOSE_LABEL.len(), CLOSE_LABEL)
                }
                cols => (cols - 2, "x"),
            };
            Button {
                col,
                label,
                control: Control::Close,
            }
        });
        let scale = close.as_ref().and_then(|close| {
            let label = options.scale.label(false);
            let (col, label) = if close.col > label.len() + 1 {
                (close.col - label.len() - 1, label)
            } else if self.cols >= 4 {
                (
                    close.col.saturating_sub(2).max(1),
                    options.scale.label(true),
                )
            } else {
                return None;
            };
            Some(Button {
                col,
                label,
                control: Control::Scale,
            })
        });
        let smoothing = scale.as_ref().and_then(|scale| {
            [options.sampling.label(false), options.sampling.label(true)]
                .into_iter()
                .find(|label| scale.col > label.len() + 1)
                .map(|label| Button {
                    col: scale.col - label.len() - 1,
                    label,
                    control: Control::Smoothing,
                })
        });
        smoothing.into_iter().chain(scale).chain(close)
    }

    pub(crate) fn control_at(
        self,
        column: u16,
        row: u16,
        options: RenderOptions,
    ) -> Option<Control> {
        let (col, row) = self.mouse_cell(column, row);
        if row != 0 || self.rows == 0 {
            return None;
        }
        self.buttons(options)
            .find(|button| (button.col..button.col + button.label.len()).contains(&col))
            .map(|button| button.control)
    }

    fn paint(self, mut out: impl Write, options: RenderOptions) -> io::Result<()> {
        write!(out, "\x1b[2J")?;
        if self.cols == 0 || self.rows == 0 {
            return Ok(());
        }
        let mut buttons = self.buttons(options).peekable();
        write!(out, "\x1b[H")?;
        let mut col = 0;
        if self.cols >= 3 {
            write!(out, "╭")?;
            col = 1;
            if buttons.peek().is_some_and(|button| button.col >= 7) {
                write!(out, " VNC ")?;
                col += " VNC ".len();
            }
        }
        let divider = if self.cols >= 3 { "─" } else { " " };
        for Button {
            col: start, label, ..
        } in buttons
        {
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

    fn paint_toggles(self, mut out: impl Write, options: RenderOptions) -> io::Result<()> {
        if self.rows > 0 {
            for Button { col, label, .. } in self
                .buttons(options)
                .filter(|button| button.control != Control::Close)
            {
                write!(out, "\x1b[1;{}H\x1b[7m{label}\x1b[27m", col + 1)?;
            }
        }
        Ok(())
    }
}

/// A renderer draws only the desktop image and reports its actual mouse hit area.
/// The surface owns toolbar drawing and the synchronized presentation boundary.
trait Renderer {
    fn preferred_mouse_coordinates(&self) -> MouseCoordinates {
        MouseCoordinates::Cells
    }

    fn draw(
        &mut self,
        surface: RenderSurface<'_>,
        frame: Option<&Frame>,
    ) -> io::Result<Option<Viewport>>;

    fn cleanup(&mut self, _out: &mut dyn Write) -> io::Result<()> {
        Ok(())
    }
}

fn renderer_for(mode: RenderMode) -> Box<dyn Renderer> {
    match mode {
        RenderMode::Kitty => Box::new(KittyRenderer::new()),
        RenderMode::Text => Box::new(TextRenderer::default()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Chrome {
    pane: Pane,
    options: RenderOptions,
}

enum ChromeUpdate {
    Repaint,
    TogglesOnly,
    Unchanged,
}

struct RenderSurface<'a> {
    out: &'a mut dyn Write,
    pane: Pane,
    options: RenderOptions,
    chrome: ChromeUpdate,
}

impl<'a> RenderSurface<'a> {
    fn new(out: &'a mut dyn Write, current: Chrome, previous: Option<Chrome>) -> Self {
        let chrome = match previous {
            Some(previous) if previous == current => ChromeUpdate::Unchanged,
            Some(previous) if previous.pane == current.pane => ChromeUpdate::TogglesOnly,
            _ => ChromeUpdate::Repaint,
        };
        Self {
            out,
            pane: current.pane,
            options: current.options,
            chrome,
        }
    }

    fn needs_repaint(&self) -> bool {
        matches!(self.chrome, ChromeUpdate::Repaint)
    }

    /// Output for a renderer that uploads before presenting. A full screen
    /// repaint must come first because an erase may discard an uploaded image.
    fn prepare_output(&mut self) -> io::Result<&mut dyn Write> {
        if self.needs_repaint() {
            self.pane.paint(&mut self.out, self.options)?;
            self.chrome = ChromeUpdate::Unchanged;
        }
        Ok(self.out)
    }

    fn present(self, draw: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> io::Result<()> {
        self.out.write_all(b"\x1b[?2026h")?;
        let result = (|| {
            match self.chrome {
                ChromeUpdate::Repaint => self.pane.paint(&mut *self.out, self.options)?,
                ChromeUpdate::TogglesOnly => {
                    self.pane.paint_toggles(&mut *self.out, self.options)?
                }
                ChromeUpdate::Unchanged => {}
            }
            draw(self.out)
        })();
        let end = self.out.write_all(b"\x1b[?2026l");
        result.and(end)
    }
}

/// Platform terminal operations, separate from drawing so the session can
/// exercise real viewer behavior against an in-memory terminal.
pub(crate) trait TerminalIo {
    fn writer(&mut self) -> impl Write;
    fn window_size(&self) -> io::Result<terminal::WindowSize>;
    fn set_raw_mode(&mut self, enabled: bool) -> io::Result<()>;
}

pub(crate) struct Console(Stdout);

impl Console {
    pub(crate) fn new() -> Self {
        Self(io::stdout())
    }
}

impl TerminalIo for Console {
    fn writer(&mut self) -> impl Write {
        self.0.lock()
    }

    fn window_size(&self) -> io::Result<terminal::WindowSize> {
        terminal::window_size()
    }

    fn set_raw_mode(&mut self, enabled: bool) -> io::Result<()> {
        if enabled {
            terminal::enable_raw_mode()
        } else {
            terminal::disable_raw_mode()
        }
    }
}

pub(crate) struct Terminal<I: TerminalIo> {
    io: I,
    renderer: Box<dyn Renderer>,
    mouse: MouseCoordinates,
    painted: Option<Chrome>,
}

impl<I: TerminalIo> Terminal<I> {
    pub(crate) fn open(mut io: I, mode: RenderMode) -> io::Result<Self> {
        io.set_raw_mode(true)?;
        let mut screen = Self {
            io,
            renderer: renderer_for(mode),
            mouse: MouseCoordinates::Cells,
            painted: None,
        };
        {
            let mut out = screen.io.writer();
            execute!(
                out,
                terminal::EnterAlternateScreen,
                cursor::Hide,
                cursor::MoveTo(0, 0)
            )?;
            write!(out, "\x1b[?1003h\x1b[?1006h")?;
            out.flush()?;
        }
        Ok(screen)
    }

    pub(crate) fn pane(&self) -> io::Result<Pane> {
        Ok(Pane::new(
            self.io.window_size()?,
            self.renderer.preferred_mouse_coordinates(),
        ))
    }

    pub(crate) fn draw(
        &mut self,
        pane: Pane,
        frame: Option<&Frame>,
        options: RenderOptions,
    ) -> io::Result<Option<Viewport>> {
        let chrome = Chrome { pane, options };
        let mut out = io::BufWriter::new(self.io.writer());
        let view = self
            .renderer
            .draw(RenderSurface::new(&mut out, chrome, self.painted), frame)?;
        if pane.mouse != self.mouse {
            write!(
                out,
                "\x1b[?{}l\x1b[?{}h",
                self.mouse.sgr_mode(),
                pane.mouse.sgr_mode()
            )?;
            self.mouse = pane.mouse;
        }
        out.flush()?;
        self.painted = Some(chrome);
        Ok(view)
    }
}

impl<I: TerminalIo> Drop for Terminal<I> {
    fn drop(&mut self) {
        {
            let mut out = self.io.writer();
            let _ = self.renderer.cleanup(&mut out);
            let _ = write!(out, "\x1b[0m\x1b[?2026l\x1b[?1016l\x1b[?1006l\x1b[?1003l");
            let _ = execute!(out, cursor::Show, terminal::LeaveAlternateScreen);
        }
        let _ = self.io.set_raw_mode(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderers_share_a_surface_but_choose_their_own_output_and_mouse_units() {
        let size = terminal::WindowSize {
            columns: 40,
            rows: 20,
            width: 320,
            height: 320,
        };
        let frame = Frame::new(16, 10);
        for (mode, mouse) in [
            (RenderMode::Kitty, MouseCoordinates::Pixels),
            (RenderMode::Text, MouseCoordinates::Cells),
        ] {
            let mut renderer = renderer_for(mode);
            let pane = Pane::new(
                terminal::WindowSize { ..size },
                renderer.preferred_mouse_coordinates(),
            );
            assert_eq!(pane.mouse, mouse);
            assert_eq!((pane.cell_width, pane.cell_height), (8, 16));
            let chrome = Chrome {
                pane,
                options: RenderOptions::default(),
            };
            let mut out = Vec::new();
            let view = renderer
                .draw(RenderSurface::new(&mut out, chrome, None), Some(&frame))
                .unwrap();
            assert!(view.is_some());
            renderer.cleanup(&mut out).unwrap();
            let out = String::from_utf8(out).unwrap();
            assert_eq!(out.contains("\x1b_G"), mode == RenderMode::Kitty);
            assert_eq!(out.contains('▀'), mode == RenderMode::Text);
            assert!(out.contains("[ Close ]"));
        }
    }

    #[test]
    fn toolbar_hitboxes_track_the_painted_buttons_in_pixels() {
        let pane = Pane::new(
            terminal::WindowSize {
                columns: 80,
                rows: 24,
                width: 800,
                height: 480,
            },
            MouseCoordinates::Pixels,
        );
        let mut border = Vec::new();
        pane.paint(
            &mut border,
            RenderOptions {
                scale: ScaleMode::Fill,
                ..RenderOptions::default()
            },
        )
        .unwrap();
        assert!(
            String::from_utf8(border)
                .unwrap()
                .contains("\x1b[7m[ Fill ]\x1b[27m─\x1b[7m[ Close ]\x1b[27m╮")
        );
        for scale in [ScaleMode::Fit, ScaleMode::Fill] {
            for (x, y, hit) in [
                (449, 19, None),
                (450, 19, Some(Control::Smoothing)),
                (599, 19, Some(Control::Smoothing)),
                (609, 19, None),
                (610, 19, Some(Control::Scale)),
                (689, 19, Some(Control::Scale)),
                (699, 19, None),
                (700, 19, Some(Control::Close)),
                (789, 19, Some(Control::Close)),
                (790, 19, None),
                (700, 20, None),
            ] {
                for sampling in [Sampling::Smooth, Sampling::Nearest] {
                    assert_eq!(
                        pane.control_at(x, y, RenderOptions { scale, sampling }),
                        hit
                    );
                }
            }
        }
    }

    #[test]
    fn tiny_panes_keep_close_without_a_desktop_area() {
        use geometry::RasterSize;

        for cols in 1..32 {
            let pane = Pane::new(
                terminal::WindowSize {
                    columns: cols,
                    rows: 2,
                    width: 0,
                    height: 0,
                },
                MouseCoordinates::Pixels,
            );
            let options = RenderOptions::default();
            pane.paint(Vec::new(), options).unwrap();
            let hits: Vec<_> = (0..cols)
                .filter_map(|col| pane.control_at(col, 0, options))
                .collect();
            assert!(hits.contains(&Control::Close));
            assert_eq!(hits.contains(&Control::Scale), cols >= 4);
            assert!(
                pane.fit(
                    RasterSize {
                        width: 320,
                        height: 192
                    },
                    ScaleMode::Fit
                )
                .is_none()
            );
        }
    }
}
