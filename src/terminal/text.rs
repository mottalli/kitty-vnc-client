use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CellRect {
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Default)]
pub(super) struct TextRenderer {
    previous: Option<(CellRect, Vec<u8>)>,
}

impl TextRenderer {
    pub fn invalidate(&mut self) {
        self.previous = None;
    }

    pub fn draw(
        &mut self,
        mut out: impl Write,
        image: Option<(CellRect, Vec<u8>)>,
    ) -> io::Result<()> {
        if let Some((old, _)) = &self.previous
            && image.as_ref().map(|(rect, _)| rect) != Some(old)
        {
            // Fit/Fill and desktop resolution changes can move or shrink the image.
            // Erase only the old rectangle, not the toolbar or frame.
            for row in old.row..old.row + old.rows {
                write!(out, "\x1b[{};{}H\x1b[{}X", row + 1, old.col + 1, old.cols)?;
            }
            self.invalidate();
        }
        let Some((rect, pixels)) = image else {
            return Ok(());
        };
        let rgb = pixels.as_chunks::<3>().0;
        let previous = self
            .previous
            .as_ref()
            .map(|(_, pixels)| pixels.as_chunks::<3>().0);
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
                    color(&mut out, 38, fg)?;
                    foreground = Some(fg);
                }
                if background != Some(bg) {
                    color(&mut out, 48, bg)?;
                    background = Some(bg);
                }
                out.write_all("▀".as_bytes())?;
            }
        }
        if foreground.is_some() {
            out.write_all(b"\x1b[0m")?;
        }
        self.previous = Some((rect, pixels));
        Ok(())
    }
}

fn color(mut out: impl Write, kind: u8, [r, g, b]: [u8; 3]) -> io::Result<()> {
    write!(out, "\x1b[{kind};2;{r};{g};{b}m")
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .draw(&mut out, Some((rect, pixels.clone())))
            .unwrap();
        assert_eq!(
            out,
            "\x1b[2;2H\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀▀\x1b[0m".as_bytes()
        );
        out.clear();
        renderer
            .draw(&mut out, Some((rect, pixels.clone())))
            .unwrap();
        assert!(out.is_empty());
        pixels[9..12].copy_from_slice(&[0, 255, 0]);
        renderer.draw(&mut out, Some((rect, pixels))).unwrap();
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
        renderer.draw(io::sink(), Some((old, vec![0; 12]))).unwrap();
        let mut out = Vec::new();
        renderer.draw(&mut out, Some((new, vec![0; 12]))).unwrap();
        assert!(out.starts_with(b"\x1b[5;4H\x1b[2X\x1b[5;2H"));
        assert!(out.ends_with("▀▀\x1b[0m".as_bytes()));
        out.clear();
        renderer.draw(&mut out, None).unwrap();
        assert_eq!(out, b"\x1b[5;2H\x1b[2X");
    }
}
