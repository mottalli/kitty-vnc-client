use std::io::{self, Write};

use crate::Frame;

use super::geometry::{CellRect, RasterSize, ScreenArea};
use super::{Pane, RenderSurface, Renderer, ScaleMode, Viewport};

#[derive(Clone, Copy)]
struct TextPlacement {
    cells: CellRect,
    desktop: RasterSize,
}

impl TextPlacement {
    fn new(pane: Pane, desktop: RasterSize, scale: ScaleMode) -> Option<Self> {
        let fitted = pane.fit(desktop, scale)?;
        // Fit in physical units first, then round to the nearest whole cell.
        let cols = ((fitted.raster.width + pane.cell_width / 2) / pane.cell_width)
            .clamp(1, fitted.interior.cols);
        let rows = ((fitted.raster.height + pane.cell_height / 2) / pane.cell_height)
            .clamp(1, fitted.interior.rows);
        Some(Self {
            cells: fitted.interior.centered(cols, rows),
            desktop,
        })
    }

    fn raster(self) -> RasterSize {
        RasterSize {
            width: self.cells.cols,
            height: self.cells.rows * 2,
        }
    }

    fn viewport(self) -> Viewport {
        Viewport::new(ScreenArea::Cells(self.cells), self.desktop)
    }
}

struct TextImage {
    cells: CellRect,
    rgb: Vec<u8>,
}

impl TextImage {
    fn samples(&self) -> &[[u8; 3]] {
        self.rgb.as_chunks::<3>().0
    }
}

#[derive(Default)]
pub(super) struct TextRenderer {
    previous: Option<TextImage>,
}

impl Renderer for TextRenderer {
    fn draw(
        &mut self,
        surface: RenderSurface<'_>,
        frame: Option<&Frame>,
    ) -> io::Result<Option<Viewport>> {
        let placement = frame.and_then(|frame| {
            TextPlacement::new(surface.pane, frame.into(), surface.options.scale)
        });
        let image = match (placement, frame) {
            (Some(placement), Some(frame)) => {
                let RasterSize { width, height } = placement.raster();
                Some(TextImage {
                    cells: placement.cells,
                    rgb: frame.scaled_rgb(width, height, surface.options.sampling)?,
                })
            }
            _ => None,
        };
        if surface.needs_repaint() {
            self.previous = None;
        }
        surface.present(|out| self.paint_image(out, image))?;
        Ok(placement.map(TextPlacement::viewport))
    }
}

impl TextRenderer {
    fn paint_image(&mut self, mut out: impl Write, image: Option<TextImage>) -> io::Result<()> {
        if let Some(old) = self.previous.as_ref().map(|image| image.cells)
            && image.as_ref().map(|image| image.cells) != Some(old)
        {
            // Fit/Fill and desktop resolution changes can move or shrink the image.
            // Erase only the old rectangle, not the toolbar or frame.
            for row in old.row..old.row + old.rows {
                write!(out, "\x1b[{};{}H\x1b[{}X", row + 1, old.col + 1, old.cols)?;
            }
            self.previous = None;
        }
        let Some(image) = image else {
            return Ok(());
        };
        let rect = image.cells;
        let rgb = image.samples();
        let previous = self.previous.as_ref().map(TextImage::samples);
        let mut foreground = None;
        let mut background = None;
        for row in 0..rect.rows {
            let top = row * 2 * rect.cols;
            let bottom = top + rect.cols;
            let mut in_run = false;
            for col in 0..rect.cols {
                let fg = rgb[top + col];
                let bg = rgb[bottom + col];
                if previous.is_some_and(|prev| prev[top + col] == fg && prev[bottom + col] == bg) {
                    in_run = false;
                    continue;
                }
                if !in_run {
                    write!(out, "\x1b[{};{}H", rect.row + row + 1, rect.col + col + 1)?;
                    in_run = true;
                }
                if foreground != Some(fg) {
                    color(&mut out, ColorLayer::Foreground, fg)?;
                    foreground = Some(fg);
                }
                if background != Some(bg) {
                    color(&mut out, ColorLayer::Background, bg)?;
                    background = Some(bg);
                }
                out.write_all("▀".as_bytes())?;
            }
        }
        if foreground.is_some() {
            out.write_all(b"\x1b[0m")?;
        }
        self.previous = Some(image);
        Ok(())
    }
}

enum ColorLayer {
    Foreground,
    Background,
}

fn color(mut out: impl Write, layer: ColorLayer, [r, g, b]: [u8; 3]) -> io::Result<()> {
    let code = match layer {
        ColorLayer::Foreground => 38,
        ColorLayer::Background => 48,
    };
    write!(out, "\x1b[{code};2;{r};{g};{b}m")
}

#[cfg(test)]
mod tests {
    use super::super::geometry::MouseCoordinates;
    use super::*;
    use crossterm::terminal::WindowSize;

    fn image(cells: CellRect, rgb: Vec<u8>) -> Option<TextImage> {
        Some(TextImage { cells, rgb })
    }

    #[test]
    fn fit_uses_cell_shape_and_maps_cell_center_clicks() {
        let pane = Pane::new(
            WindowSize {
                columns: 82,
                rows: 42,
                width: 820,
                height: 1260,
            },
            MouseCoordinates::Cells,
        );
        let desktop = RasterSize {
            width: 800,
            height: 600,
        };
        let fit = TextPlacement::new(pane, desktop, ScaleMode::Fit).unwrap();
        assert_eq!(
            fit.cells,
            CellRect {
                col: 1,
                row: 11,
                cols: 80,
                rows: 20
            }
        );
        assert_eq!(
            fit.raster(),
            RasterSize {
                width: 80,
                height: 40
            }
        );
        assert_eq!(fit.viewport().point(1, 11, false), Some((5, 15)));
        assert_eq!(fit.viewport().point(1, 10, false), None);

        let fill = TextPlacement::new(pane, desktop, ScaleMode::Fill).unwrap();
        assert_eq!(
            fill.cells,
            CellRect {
                col: 1,
                row: 1,
                cols: 80,
                rows: 40
            }
        );
        assert_eq!(
            fill.raster(),
            RasterSize {
                width: 80,
                height: 80
            }
        );
        assert_eq!(fill.viewport().point(80, 40, false), Some((795, 592)));
        assert_eq!(fill.viewport().point(80, 41, false), None);
    }

    #[test]
    fn pairs_top_and_bottom_colors_and_skips_unchanged_cells() {
        let rect = CellRect {
            col: 1,
            row: 1,
            cols: 2,
            rows: 1,
        };
        let mut renderer = TextRenderer::default();
        let mut pixels = vec![255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255];
        let mut out = Vec::new();
        renderer
            .paint_image(&mut out, image(rect, pixels.clone()))
            .unwrap();
        assert_eq!(
            out,
            "\x1b[2;2H\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀▀\x1b[0m".as_bytes()
        );
        out.clear();
        renderer
            .paint_image(&mut out, image(rect, pixels.clone()))
            .unwrap();
        assert!(out.is_empty());
        pixels[9..12].copy_from_slice(&[0, 255, 0]);
        renderer.paint_image(&mut out, image(rect, pixels)).unwrap();
        assert_eq!(
            out,
            "\x1b[2;3H\x1b[38;2;255;0;0m\x1b[48;2;0;255;0m▀\x1b[0m".as_bytes()
        );
    }

    #[test]
    fn moving_the_image_erases_old_cells_before_redrawing() {
        let old = CellRect {
            col: 3,
            row: 4,
            cols: 2,
            rows: 1,
        };
        let new = CellRect { col: 1, ..old };
        let mut renderer = TextRenderer::default();
        renderer
            .paint_image(io::sink(), image(old, vec![0; 12]))
            .unwrap();
        let mut out = Vec::new();
        renderer
            .paint_image(&mut out, image(new, vec![0; 12]))
            .unwrap();
        assert!(out.starts_with(b"\x1b[5;4H\x1b[2X\x1b[5;2H"));
        assert!(out.ends_with("▀▀\x1b[0m".as_bytes()));
        out.clear();
        renderer.paint_image(&mut out, None).unwrap();
        assert_eq!(out, b"\x1b[5;2H\x1b[2X");
    }
}
