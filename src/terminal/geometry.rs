use crossterm::terminal::WindowSize;

use crate::Frame;

use super::ScaleMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseCoordinates {
    Cells,
    Pixels,
}

impl MouseCoordinates {
    pub(super) fn sgr_mode(self) -> u16 {
        match self {
            Self::Cells => 1006,
            Self::Pixels => 1016,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pane {
    pub(super) cols: usize,
    pub(super) rows: usize,
    pub(super) cell_width: usize,
    pub(super) cell_height: usize,
    pub(super) mouse: MouseCoordinates,
}

impl Pane {
    pub(super) fn new(size: WindowSize, preferred_mouse: MouseCoordinates) -> Self {
        let (cols, rows) = (usize::from(size.columns), usize::from(size.rows));
        let cell_width = usize::from(size.width) / cols.max(1);
        let cell_height = usize::from(size.height) / rows.max(1);
        let (cell_width, cell_height, mouse) = if cell_width > 0 && cell_height > 0 {
            (cell_width, cell_height, preferred_mouse)
        } else {
            (8, 16, MouseCoordinates::Cells)
        };
        Self {
            cols,
            rows,
            cell_width,
            cell_height,
            mouse,
        }
    }

    pub(super) fn mouse_cell(self, column: u16, row: u16) -> (usize, usize) {
        let (x, y) = (usize::from(column), usize::from(row));
        match self.mouse {
            MouseCoordinates::Pixels => (x / self.cell_width, y / self.cell_height),
            MouseCoordinates::Cells => (x, y),
        }
    }

    pub(super) fn fit(self, desktop: RasterSize, scale: ScaleMode) -> Option<FittedDesktop> {
        if self.cols < 3 || self.rows < 3 {
            return None;
        }
        let interior = CellRect {
            col: 1,
            row: 1,
            cols: self.cols - 2,
            rows: self.rows - 2,
        };
        let available = RasterSize {
            width: interior.cols * self.cell_width,
            height: interior.rows * self.cell_height,
        };
        let raster = match scale {
            ScaleMode::Stretch => available,
            ScaleMode::Proportional
                if available.width * desktop.height <= available.height * desktop.width =>
            {
                RasterSize {
                    width: available.width,
                    height: (available.width * desktop.height / desktop.width).max(1),
                }
            }
            ScaleMode::Proportional => RasterSize {
                width: (available.height * desktop.width / desktop.height).max(1),
                height: available.height,
            },
        };
        Some(FittedDesktop {
            interior,
            available,
            raster,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RasterSize {
    pub width: usize,
    pub height: usize,
}

impl From<&Frame> for RasterSize {
    fn from(frame: &Frame) -> Self {
        Self {
            width: frame.width,
            height: frame.height,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CellRect {
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
}

impl CellRect {
    pub(super) fn centered(self, cols: usize, rows: usize) -> Self {
        Self {
            col: self.col + (self.cols - cols) / 2,
            row: self.row + (self.rows - rows) / 2,
            cols,
            rows,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PixelRect {
    pub left: usize,
    pub top: usize,
    pub size: RasterSize,
}

pub(super) struct FittedDesktop {
    pub interior: CellRect,
    pub available: RasterSize,
    pub raster: RasterSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScreenArea {
    Cells(CellRect),
    Pixels(PixelRect),
}

/// The area the renderer actually drew, used to map mouse reports to desktop pixels.
#[derive(Clone, Copy)]
pub(crate) struct Viewport {
    area: ScreenArea,
    desktop: RasterSize,
}

impl Viewport {
    pub(super) fn new(area: ScreenArea, desktop: RasterSize) -> Self {
        Self { area, desktop }
    }

    pub(crate) fn point(self, column: u16, row: u16, clamp: bool) -> Option<(u16, u16)> {
        let (left, top, width, height) = match self.area {
            ScreenArea::Cells(rect) => (rect.col, rect.row, rect.cols, rect.rows),
            ScreenArea::Pixels(rect) => (rect.left, rect.top, rect.size.width, rect.size.height),
        };
        let (x, y) = (usize::from(column), usize::from(row));
        if !clamp && (x < left || y < top || x >= left + width || y >= top + height) {
            return None;
        }
        let scale = |value: usize, extent: usize, desktop: usize| {
            let value = value.min(extent - 1);
            // Without pixel reports, approximate the click at the center of its cell.
            let offset = if matches!(self.area, ScreenArea::Cells(_)) {
                desktop / 2
            } else {
                0
            };
            ((value * desktop + offset) / extent) as u16
        };
        Some((
            scale(x.saturating_sub(left), width, self.desktop.width),
            scale(y.saturating_sub(top), height, self.desktop.height),
        ))
    }
}
