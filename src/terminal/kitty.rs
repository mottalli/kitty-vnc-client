use std::io::{self, Write};

use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::{Compression, write::ZlibEncoder};

use crate::Frame;

use super::geometry::{MouseCoordinates, PixelRect, RasterSize, ScreenArea};
use super::{Pane, RenderSurface, Renderer, Sampling, ScaleMode, Viewport};

#[derive(Clone, Copy)]
struct ImageId(u32);

pub(super) struct KittyRenderer {
    next: ImageId,
    displayed: ImageId,
}

impl KittyRenderer {
    pub(super) fn new() -> Self {
        let id = (std::process::id() & 0x7fff_ffff).max(1) * 2;
        Self {
            next: ImageId(id),
            displayed: ImageId(id + 1),
        }
    }
}

impl Renderer for KittyRenderer {
    fn preferred_mouse_coordinates(&self) -> MouseCoordinates {
        MouseCoordinates::Pixels
    }

    fn draw(
        &mut self,
        mut surface: RenderSurface<'_>,
        frame: Option<&Frame>,
    ) -> io::Result<Option<Viewport>> {
        let pane = surface.pane;
        let options = surface.options;
        let placement =
            frame.and_then(|frame| KittyPlacement::new(pane, frame.into(), options.scale));

        // Upload outside synchronized output; the old image remains visible
        // until the new placement is ready.
        let out = surface.prepare_output()?;
        if let (Some(placement), Some(frame)) = (placement, frame) {
            upload(out, self.next, placement.raster, frame, options.sampling)?;
        }
        surface.present(|out| {
            if let Some(placement) = placement {
                placement.place(out, pane, self.next)?;
            }
            delete_image(out, self.displayed)
        })?;
        if placement.is_some() {
            std::mem::swap(&mut self.next, &mut self.displayed);
        }
        Ok(placement.map(KittyPlacement::viewport))
    }

    fn cleanup(&mut self, out: &mut dyn Write) -> io::Result<()> {
        // Terminate a graphics command if an I/O error interrupted its payload.
        out.write_all(b"\x1b\\")?;
        delete_image(out, self.next)?;
        delete_image(out, self.displayed)
    }
}

#[derive(Clone, Copy)]
struct KittyPlacement {
    area: ScreenArea,
    raster: RasterSize,
    desktop: RasterSize,
}

impl KittyPlacement {
    fn new(pane: Pane, desktop: RasterSize, scale: ScaleMode) -> Option<Self> {
        let fitted = pane.fit(desktop, scale)?;
        let raster = fitted.raster;
        let area = match pane.mouse {
            MouseCoordinates::Pixels => ScreenArea::Pixels(PixelRect {
                left: fitted.interior.col * pane.cell_width
                    + (fitted.available.width - raster.width) / 2,
                top: fitted.interior.row * pane.cell_height
                    + (fitted.available.height - raster.height) / 2,
                size: raster,
            }),
            MouseCoordinates::Cells => ScreenArea::Cells(fitted.interior.centered(
                raster.width.div_ceil(pane.cell_width),
                raster.height.div_ceil(pane.cell_height),
            )),
        };
        Some(Self {
            area,
            raster,
            desktop,
        })
    }

    fn viewport(self) -> Viewport {
        Viewport::new(self.area, self.desktop)
    }

    fn place(self, out: &mut dyn Write, pane: Pane, id: ImageId) -> io::Result<()> {
        let ImageId(id) = id;
        match self.area {
            ScreenArea::Cells(rect) => write!(
                out,
                "\x1b[{};{}H\x1b_Ga=p,i={id},C=1,q=2,c={},r={};\x1b\\",
                rect.row + 1,
                rect.col + 1,
                rect.cols,
                rect.rows,
            ),
            ScreenArea::Pixels(rect) => write!(
                out,
                "\x1b[{};{}H\x1b_Ga=p,i={id},C=1,q=2,X={},Y={};\x1b\\",
                rect.top / pane.cell_height + 1,
                rect.left / pane.cell_width + 1,
                rect.left % pane.cell_width,
                rect.top % pane.cell_height,
            ),
        }
    }
}

fn delete_image(out: &mut dyn Write, ImageId(id): ImageId) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")
}

fn upload(
    out: &mut dyn Write,
    ImageId(id): ImageId,
    RasterSize { width, height }: RasterSize,
    frame: &Frame,
    sampling: Sampling,
) -> io::Result<()> {
    let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
    compressor.write_all(&frame.scaled_rgb(width, height, sampling)?)?;
    let payload = STANDARD.encode(compressor.finish()?);
    let chunks = payload.as_bytes().chunks(4096);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let more = u8::from(index + 1 < count);
        if index == 0 {
            write!(
                out,
                "\x1b_Ga=t,f=24,o=z,s={width},v={height},i={id},q=2,m={more};"
            )?;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{Chrome, RenderOptions};
    use super::*;
    use crossterm::terminal::WindowSize;

    fn pane(columns: u16, rows: u16, width: u16, height: u16) -> Pane {
        Pane::new(
            WindowSize {
                columns,
                rows,
                width,
                height,
            },
            MouseCoordinates::Pixels,
        )
    }

    fn renderer() -> KittyRenderer {
        KittyRenderer {
            next: ImageId(2),
            displayed: ImageId(1),
        }
    }

    #[test]
    fn scale_toggle_replaces_image_without_clearing_the_screen() {
        let pane = pane(40, 20, 320, 320);
        let frame = Frame::new(16, 10);
        for old_scale in [ScaleMode::Fit, ScaleMode::Fill] {
            let scale = old_scale.toggled();
            let options = RenderOptions {
                scale,
                ..RenderOptions::default()
            };
            let before = Chrome {
                pane,
                title: "Test Driver",
                options: RenderOptions {
                    scale: old_scale,
                    ..options
                },
            };
            let mut output = Vec::new();
            renderer()
                .draw(
                    RenderSurface::new(&mut output, Chrome { options, ..before }, Some(before)),
                    Some(&frame),
                )
                .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(!output.contains("\x1b[2J"));
            assert!(output.contains(scale.label(false)));
            let upload = output.find("\x1b_Ga=t,").unwrap();
            let sync = output.find("\x1b[?2026h").unwrap();
            let place = output.find("\x1b_Ga=p,i=2,").unwrap();
            let delete = output.find("\x1b_Ga=d,d=I,i=1,").unwrap();
            assert!(upload < sync && sync < place && place < delete);
        }
    }

    #[test]
    fn resize_clears_before_uploading_the_replacement_image() {
        let before = Chrome {
            pane: pane(40, 20, 320, 320),
            options: RenderOptions::default(),
            title: "Test Driver",
        };
        let current = Chrome {
            pane: pane(30, 20, 240, 320),
            ..before
        };
        let frame = Frame::new(16, 10);
        let mut output = Vec::new();
        renderer()
            .draw(
                RenderSurface::new(&mut output, current, Some(before)),
                Some(&frame),
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
        let pane = pane(80, 24, 800, 480);
        let place = KittyPlacement::new(
            pane,
            RasterSize {
                width: 1600,
                height: 900,
            },
            ScaleMode::Fit,
        )
        .unwrap();
        assert_eq!(
            place.area,
            ScreenArea::Pixels(PixelRect {
                left: 10,
                top: 21,
                size: RasterSize {
                    width: 780,
                    height: 438
                }
            })
        );
        let view = place.viewport();
        assert_eq!(view.point(400, 240, false), Some((800, 450)));
        assert_eq!(view.point(400, 20, false), None);
        assert_eq!(view.point(9, 240, false), None);
        assert_eq!(view.point(400, 459, false), None);
        assert_eq!(view.point(400, 459, true), Some((800, 897)));
        assert_eq!(view.point(0, 0, true), Some((0, 0)));

        let mut output = Vec::new();
        place.place(&mut output, pane, ImageId(7)).unwrap();
        assert_eq!(output, b"\x1b[2;2H\x1b_Ga=p,i=7,C=1,q=2,X=0,Y=1;\x1b\\");
    }

    #[test]
    fn scales_up_a_smaller_desktop_and_maps_clicks_in_the_added_area() {
        let pane = pane(100, 60, 1600, 1320);
        let place = KittyPlacement::new(
            pane,
            RasterSize {
                width: 1280,
                height: 1024,
            },
            ScaleMode::Fit,
        )
        .unwrap();
        assert_eq!(
            place.area,
            ScreenArea::Pixels(PixelRect {
                left: 16,
                top: 33,
                size: RasterSize {
                    width: 1568,
                    height: 1254
                }
            })
        );
        let view = place.viewport();
        assert_eq!(view.point(800, 660, false), Some((640, 512)));
        assert!(view.point(1500, 1200, false).is_some());
        assert_eq!(view.point(1500, 1287, false), None);

        let portrait = KittyPlacement::new(
            pane,
            RasterSize {
                width: 600,
                height: 800,
            },
            ScaleMode::Fit,
        )
        .unwrap();
        assert_eq!(
            portrait.area,
            ScreenArea::Pixels(PixelRect {
                left: 321,
                top: 22,
                size: RasterSize {
                    width: 957,
                    height: 1276
                }
            })
        );
        assert_eq!(
            portrait.viewport().point(1120, 660, false),
            Some((500, 400))
        );
        assert_eq!(portrait.viewport().point(1278, 660, false), None);
    }

    #[test]
    fn falls_back_to_cells_without_pixel_dimensions() {
        use super::super::geometry::CellRect;
        let pane = pane(80, 24, 0, 0);
        let desktop = RasterSize {
            width: 320,
            height: 192,
        };
        let place = KittyPlacement::new(pane, desktop, ScaleMode::Fit).unwrap();
        assert_eq!(
            place.raster,
            RasterSize {
                width: 586,
                height: 352
            }
        );
        assert_eq!(
            place.area,
            ScreenArea::Cells(CellRect {
                col: 3,
                row: 1,
                cols: 74,
                rows: 22
            })
        );
        assert_eq!(place.viewport().point(3, 1, false), Some((2, 4)));
        assert_eq!(place.viewport().point(2, 1, false), None);
        let options = RenderOptions::default();
        assert_eq!(
            pane.control_at(76, 0, options),
            Some(super::super::Control::Close)
        );
        assert_eq!(pane.control_at(76, 1, options), None);

        let fill = KittyPlacement::new(pane, desktop, ScaleMode::Fill).unwrap();
        assert_eq!(
            fill.area,
            ScreenArea::Cells(CellRect {
                col: 1,
                row: 1,
                cols: 78,
                rows: 22
            })
        );
    }

    #[test]
    fn fill_covers_the_interior_and_maps_each_axis_independently() {
        let pane = pane(80, 24, 800, 480);
        let scale = ScaleMode::Fit.toggled();
        assert_eq!(scale.toggled(), ScaleMode::Fit);
        let place = KittyPlacement::new(
            pane,
            RasterSize {
                width: 320,
                height: 100,
            },
            scale,
        )
        .unwrap();
        assert_eq!(
            place.area,
            ScreenArea::Pixels(PixelRect {
                left: 10,
                top: 20,
                size: RasterSize {
                    width: 780,
                    height: 440
                }
            })
        );
        let view = place.viewport();
        assert_eq!(view.point(10, 20, false), Some((0, 0)));
        assert_eq!(view.point(400, 240, false), Some((160, 50)));
        assert_eq!(view.point(789, 459, false), Some((319, 99)));
        assert_eq!(view.point(789, 460, false), None);
    }
}
