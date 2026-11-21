use std::io::{self, IsTerminal};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use crossterm::event::{EventStream, KeyEvent, KeyModifiers};
use futures_util::StreamExt;
use terminal_vnc_kitty::{
    Frame, KEY_MODIFIERS, PointerUpdate, Viewer, ViewerEvent, key_tap, parse_viewer_args,
};
use tokio::time::{self, MissedTickBehavior};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageOrder, VisualClass};
use x11rb::protocol::xtest::ConnectionExt as _;
#[cfg(unix)]
use x11rb::rust_connection::DefaultStream;
use x11rb::rust_connection::RustConnection;
#[cfg(unix)]
use x11rb_protocol::xauth::get_auth;

fn connect_x11(display: Option<&str>) -> Result<(RustConnection, usize)> {
    #[cfg(unix)]
    {
        let from_env = std::env::var("DISPLAY").ok();
        if let Some(path) = display
            .or(from_env.as_deref())
            .filter(|arg| arg.starts_with('/'))
        {
            // x11rb's normal connector resolves Unix displays to /tmp/.X11-unix.
            // Open custom paths ourselves while keeping the standard Xauthority lookup.
            let stream = UnixStream::connect(path)
                .with_context(|| format!("could not open X11 socket {path}"))?;
            let (stream, (family, address)) = DefaultStream::from_unix_stream(stream)?;
            let auth = match get_auth(family, &address, 0) {
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && std::env::var_os("XAUTHORITY").is_none() =>
                {
                    None
                }
                result => result.context("could not read X11 authorization")?,
            };
            let (name, data) = auth.unwrap_or_default();
            let conn = RustConnection::connect_to_stream_with_auth_info(stream, 0, name, data)
                .context("X11 setup failed on the Unix socket (check XAUTHORITY for display 0)")?;
            return Ok((conn, 0));
        }
    }
    x11rb::connect(display).context("could not connect to X11 (check DISPLAY and XAUTHORITY)")
}

struct PixelFormat {
    bytes_per_pixel: usize,
    pad: usize,
    little_endian: bool,
    masks: [u32; 3],
}

impl PixelFormat {
    fn decode(&self, width: usize, height: usize, data: &[u8], frame: &mut Frame) -> Result<()> {
        let row_bytes = width * self.bytes_per_pixel;
        let stride = row_bytes.div_ceil(self.pad) * self.pad;
        ensure!(data.len() >= stride * height, "short X11 image reply");
        let shifts = self.masks.map(u32::trailing_zeros);
        let max = std::array::from_fn::<_, 3, _>(|i| u64::from(self.masks[i] >> shifts[i]));
        for (row, out) in data
            .chunks_exact(stride)
            .zip(frame.pixels_mut().chunks_exact_mut(width * 4))
        {
            for (src, dst) in row[..row_bytes]
                .chunks_exact(self.bytes_per_pixel)
                .zip(out.as_chunks_mut::<4>().0)
            {
                let mut bytes = [0; 4];
                let value = if self.little_endian {
                    bytes[..src.len()].copy_from_slice(src);
                    u32::from_le_bytes(bytes)
                } else {
                    bytes[4 - src.len()..].copy_from_slice(src);
                    u32::from_be_bytes(bytes)
                };
                for i in 0..3 {
                    let component = u64::from((value & self.masks[i]) >> shifts[i]);
                    dst[i] = ((component * 255 + max[i] / 2) / max[i]) as u8;
                }
            }
        }
        Ok(())
    }
}

struct Keymap {
    first: u8,
    columns: usize,
    symbols: Vec<u32>,
}

impl Keymap {
    // Support the first X11 keyboard group: unshifted and Shift.
    fn find(&self, symbol: u32) -> Option<(u8, bool)> {
        for level in 0..self.columns.min(2) {
            if let Some(index) = self
                .symbols
                .chunks_exact(self.columns)
                .position(|row| row[level] == symbol)
            {
                return Some((self.first + index as u8, level == 1));
            }
        }
        None
    }
}

struct XDisplay {
    conn: RustConnection,
    root: u32,
    width: u16,
    height: u16,
    format: PixelFormat,
    keymap: Keymap,
    buttons: u8,
}

impl XDisplay {
    fn connect(display: Option<&str>) -> Result<Self> {
        let (conn, screen) = connect_x11(display)?;
        conn.xtest_get_version(2, 2)?
            .reply()
            .context("X server needs XTEST for mouse and keyboard input")?;
        let setup = conn.setup();
        let screen = &setup.roots[screen];
        let width = screen.width_in_pixels;
        let height = screen.height_in_pixels;
        ensure!(
            width > 0
                && height > 0
                && u32::from(width) * u32::from(height) <= 16_777_216
                && width <= i16::MAX as u16
                && height <= i16::MAX as u16,
            "unsupported X11 screen dimensions: {width}x{height}"
        );
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|depth| &depth.visuals)
            .find(|visual| visual.visual_id == screen.root_visual)
            .context("X11 root visual not found")?;
        ensure!(
            visual.class == VisualClass::TRUE_COLOR,
            "X11 viewer needs a TrueColor display (try Xvfb with depth 24)"
        );
        let masks = [visual.red_mask, visual.green_mask, visual.blue_mask];
        ensure!(
            masks.iter().all(|mask| *mask != 0),
            "invalid X11 color masks"
        );
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == screen.root_depth)
            .context("X11 root pixel format not found")?;
        ensure!(
            matches!(format.bits_per_pixel, 16 | 24 | 32)
                && matches!(format.scanline_pad, 8 | 16 | 32),
            "unsupported X11 image format"
        );
        let format = PixelFormat {
            bytes_per_pixel: usize::from(format.bits_per_pixel / 8),
            pad: usize::from(format.scanline_pad / 8),
            little_endian: setup.image_byte_order == ImageOrder::LSB_FIRST,
            masks,
        };
        let first = setup.min_keycode;
        let keys = conn
            .get_keyboard_mapping(first, setup.max_keycode - first + 1)?
            .reply()?;
        let keymap = Keymap {
            first,
            columns: usize::from(keys.keysyms_per_keycode),
            symbols: keys.keysyms,
        };
        let root = screen.root;
        Ok(Self {
            conn,
            root,
            width,
            height,
            format,
            keymap,
            buttons: 0,
        })
    }

    fn capture(&self, frame: &mut Frame, last: &mut Vec<u8>) -> Result<bool> {
        let image = self
            .conn
            .get_image(
                xproto::ImageFormat::Z_PIXMAP,
                self.root,
                0,
                0,
                self.width,
                self.height,
                u32::MAX,
            )?
            .reply()
            .context("could not capture the X11 root window")?;
        if image.data == *last {
            return Ok(false);
        }
        self.format
            .decode(self.width.into(), self.height.into(), &image.data, frame)?;
        *last = image.data;
        Ok(true)
    }

    fn fake_input(&self, event: u8, detail: u8, x: i16, y: i16) -> Result<()> {
        self.conn
            .xtest_fake_input(event, detail, x11rb::CURRENT_TIME, self.root, x, y, 0)?
            .check()?;
        Ok(())
    }

    fn pointer(&mut self, update: PointerUpdate) -> Result<()> {
        self.fake_input(
            xproto::MOTION_NOTIFY_EVENT,
            0,
            update.x as i16,
            update.y as i16,
        )?;
        for bit in 0..3 {
            let mask = 1 << bit;
            if (self.buttons ^ update.buttons) & mask != 0 {
                let event = if update.buttons & mask != 0 {
                    xproto::BUTTON_PRESS_EVENT
                } else {
                    xproto::BUTTON_RELEASE_EVENT
                };
                self.fake_input(event, bit + 1, 0, 0)?;
                self.buttons ^= mask;
            }
        }
        if update.wheel != 0 {
            let button = update.wheel.trailing_zeros() as u8 + 1;
            self.fake_input(xproto::BUTTON_PRESS_EVENT, button, 0, 0)?;
            self.fake_input(xproto::BUTTON_RELEASE_EVENT, button, 0, 0)?;
        }
        Ok(())
    }

    fn key(&self, event: KeyEvent) -> Result<()> {
        let Some((symbol, mut modifiers)) = key_tap(event) else {
            return Ok(());
        };
        let Some((code, shifted)) = self.keymap.find(symbol) else {
            return Ok(());
        };
        if shifted {
            modifiers |= KeyModifiers::SHIFT;
        }
        let held: Vec<_> = KEY_MODIFIERS
            .into_iter()
            .filter(|(flag, _)| modifiers.contains(*flag))
            .filter_map(|(_, keysym)| self.keymap.find(keysym).map(|(code, _)| code))
            .collect();
        for &modifier in &held {
            self.fake_input(xproto::KEY_PRESS_EVENT, modifier, 0, 0)?;
        }
        self.fake_input(xproto::KEY_PRESS_EVENT, code, 0, 0)?;
        self.fake_input(xproto::KEY_RELEASE_EVENT, code, 0, 0)?;
        for modifier in held.into_iter().rev() {
            self.fake_input(xproto::KEY_RELEASE_EVENT, modifier, 0, 0)?;
        }
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let Some(args) = parse_viewer_args(&args)? else {
        println!(
            "Usage: kitty-x11 [--render kitty|text] [DISPLAY|SOCKET_PATH]\nDefaults to $DISPLAY and uses your X11 authorization.\n--render selects kitty (default) or text (colored UTF-8).\nAn absolute Unix socket path uses screen 0 and looks up display 0 in XAUTHORITY.\nClick Close or press F12 to exit."
        );
        return Ok(());
    };
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "run this directly in an interactive terminal pane"
    );
    let mut x = XDisplay::connect(args.target)?;
    let mut frame = Frame::new(x.width, x.height);
    let mut last_image = Vec::new();
    let mut viewer = Viewer::open(args.render)?;
    let mut events = EventStream::new();
    let mut tick = time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let result = async {
        loop {
            tokio::select! {
                event = events.next() => match viewer.handle(event.context("terminal input closed")??) {
                    ViewerEvent::Quit => return Ok(()),
                    ViewerEvent::Pointer(update) => x.pointer(update)?,
                    ViewerEvent::Key(key) => x.key(key)?,
                    ViewerEvent::None => {},
                },
                _ = tick.tick() => {
                    let changed = x.capture(&mut frame, &mut last_image)?;
                    if let Some(update) = viewer.render(Some(&frame), changed)? {
                        x.pointer(update)?;
                    }
                }
            }
        }
    }.await;
    if let Some(update) = viewer.reset_pointer() {
        let _ = x.pointer(update);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_x11_padding_byte_order_and_component_masks() {
        for (little_endian, data) in [
            (true, [0x1f, 0x00, 0, 0, 0x00, 0xf8, 0, 0]),
            (false, [0x00, 0x1f, 0, 0, 0xf8, 0x00, 0, 0]),
        ] {
            let format = PixelFormat {
                bytes_per_pixel: 2,
                pad: 4,
                little_endian,
                masks: [0xf800, 0x07e0, 0x001f],
            };
            let mut frame = Frame::new(1, 2);
            format.decode(1, 2, &data, &mut frame).unwrap();
            assert_eq!(frame.pixels_mut(), &[0, 0, 255, 0, 255, 0, 0, 0]);
            assert!(format.decode(1, 2, &data[..7], &mut frame).is_err());
        }
    }

    #[test]
    fn maps_keysyms_to_x11_keycodes_and_shift_level() {
        let keys = Keymap {
            first: 8,
            columns: 2,
            symbols: vec![b'a'.into(), b'A'.into(), 0xff51, 0],
        };
        assert_eq!(keys.find(b'a'.into()), Some((8, false)));
        assert_eq!(keys.find(b'A'.into()), Some((8, true)));
        assert_eq!(keys.find(0xff51), Some((9, false)));
        assert_eq!(keys.find(0xffff), None);
    }
}
