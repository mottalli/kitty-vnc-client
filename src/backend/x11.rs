mod authority;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::PathBuf;
use std::{net::Ipv6Addr, str::FromStr};

use anyhow::{Context, Result, ensure};
use crossterm::event::KeyModifiers;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageOrder, VisualClass};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb_protocol::parse_display::{ParsedDisplay, parse_display};

pub use authority::X11Authority;
use authority::{Authorization, DefaultLookup};

use crate::input::{
    ButtonSet, DesktopInput, KEY_MODIFIERS, KeyTap, PointerButton, PointerUpdate, ScrollDirection,
};
use crate::session::{self, DesktopBackend, DesktopUpdate};
use crate::{Frame, ViewerOptions};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct X11Target(X11Address);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum X11Address {
    #[default]
    Environment,
    Display(ParsedDisplay),
    #[cfg(unix)]
    UnixSocket(PathBuf),
}

impl FromStr for X11Target {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        #[cfg(unix)]
        if name.starts_with('/') {
            return Ok(Self(X11Address::UnixSocket(PathBuf::from(name))));
        }
        ensure!(
            !name.contains(['@', '?', '#']) && !name.contains(char::is_whitespace),
            "invalid X11 display; use [HOST]:DISPLAY[.SCREEN] or an absolute socket path"
        );
        let mut display = parse_display(Some(name)).context("invalid X11 display")?;
        if let Some(host) = display
            .host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
        {
            display.host = host
                .parse::<Ipv6Addr>()
                .context("invalid IPv6 X11 host")?
                .to_string();
        }
        ensure!(
            display.protocol.as_deref() == Some("unix") || display.display <= u16::MAX - 6000,
            "X11 display number is too large for a TCP port"
        );
        Ok(Self(X11Address::Display(display)))
    }
}

impl X11Target {
    fn connect(self, authority: &X11Authority) -> Result<(RustConnection, usize)> {
        match self.0 {
            X11Address::Environment => {
                let display = std::env::var("DISPLAY")
                    .context("could not read DISPLAY; specify an X display or socket path")?;
                display.parse::<Self>()?.connect(authority)
            }
            X11Address::Display(display) => connect_display(display, authority)
                .context("could not connect to X11 (check DISPLAY and X11 authorization)"),
            #[cfg(unix)]
            X11Address::UnixSocket(path) => {
                // An explicit path may be a mounted socket, not an entry in /tmp/.X11-unix.
                let stream = UnixStream::connect(&path)
                    .with_context(|| format!("could not open X11 socket {}", path.display()))?;
                let (stream, (family, address)) = DefaultStream::from_unix_stream(stream)?;
                let Authorization { name, data } =
                    authority.lookup(family, &address, 0, DefaultLookup::CheckFile)?;
                let conn = RustConnection::connect_to_stream_with_auth_info(stream, 0, name, data)
                    .context(
                        "X11 setup failed on the Unix socket (check authorization for display 0)",
                    )?;
                Ok((conn, 0))
            }
        }
    }
}

fn connect_display(
    display: ParsedDisplay,
    authority: &X11Authority,
) -> Result<(RustConnection, usize)> {
    let screen = usize::from(display.screen);
    let mut last_error = None;
    for target in display.connect_instruction() {
        match DefaultStream::connect(&target) {
            Ok((stream, (family, address))) => {
                let Authorization { name, data } = authority.lookup(
                    family,
                    &address,
                    display.display,
                    DefaultLookup::BestEffort,
                )?;
                return Ok((
                    RustConnection::connect_to_stream_with_auth_info(stream, screen, name, data)?,
                    screen,
                ));
            }
            Err(err) => last_error = Some(err),
        }
    }
    match last_error {
        Some(err) => Err(err.into()),
        None => anyhow::bail!("no connection address for X11 display"),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct X11Connection {
    pub target: X11Target,
    pub authority: X11Authority,
}

pub async fn run(connection: X11Connection, options: ViewerOptions) -> Result<()> {
    session::run(XDisplay::connect(connection)?, options).await
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
    buttons: ButtonSet,
    frame: Frame,
    last_image: Vec<u8>,
}

impl XDisplay {
    fn connect(X11Connection { target, authority }: X11Connection) -> Result<Self> {
        let (conn, screen) = target.connect(&authority)?;
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
            buttons: ButtonSet::default(),
            frame: Frame::new(width, height),
            last_image: Vec::new(),
        })
    }

    fn capture(&mut self) -> Result<bool> {
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
        if image.data == self.last_image {
            return Ok(false);
        }
        self.format.decode(
            self.width.into(),
            self.height.into(),
            &image.data,
            &mut self.frame,
        )?;
        self.last_image = image.data;
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
        for button in PointerButton::ALL {
            let pressed = update.buttons.contains(button);
            if self.buttons.contains(button) != pressed {
                let event = if pressed {
                    xproto::BUTTON_PRESS_EVENT
                } else {
                    xproto::BUTTON_RELEASE_EVENT
                };
                let detail = match button {
                    PointerButton::Left => 1,
                    PointerButton::Middle => 2,
                    PointerButton::Right => 3,
                };
                self.fake_input(event, detail, 0, 0)?;
                self.buttons.set(button, pressed);
            }
        }
        if let Some(direction) = update.scroll {
            let button = match direction {
                ScrollDirection::Up => 4,
                ScrollDirection::Down => 5,
                ScrollDirection::Left => 6,
                ScrollDirection::Right => 7,
            };
            self.fake_input(xproto::BUTTON_PRESS_EVENT, button, 0, 0)?;
            self.fake_input(xproto::BUTTON_RELEASE_EVENT, button, 0, 0)?;
        }
        Ok(())
    }

    fn key(
        &self,
        KeyTap {
            keysym,
            mut modifiers,
        }: KeyTap,
    ) -> Result<()> {
        let Some((code, shifted)) = self.keymap.find(keysym) else {
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

impl DesktopBackend for XDisplay {
    fn frame_title(&self) -> &'static str {
        "X11"
    }

    fn frame(&self) -> Option<&Frame> {
        Some(&self.frame)
    }

    async fn refresh(&mut self) -> Result<DesktopUpdate> {
        Ok(if self.capture()? {
            DesktopUpdate::FrameChanged
        } else {
            DesktopUpdate::Unchanged
        })
    }

    async fn send_input(&mut self, input: DesktopInput) -> Result<()> {
        match input {
            DesktopInput::Key(key) => self.key(key),
            DesktopInput::Pointer(pointer) => self.pointer(pointer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_display_address_components_and_rejects_invalid_displays() {
        let X11Address::Display(display) = "localhost:12.1".parse::<X11Target>().unwrap().0 else {
            panic!("expected a display address");
        };
        assert_eq!(display.host, "localhost");
        assert_eq!(display.display, 12);
        assert_eq!(display.screen, 1);
        let X11Address::Display(ipv6) = "[::1]:99".parse::<X11Target>().unwrap().0 else {
            panic!("expected an IPv6 display address");
        };
        assert_eq!(ipv6.host, "::1");
        for display in ["localhost", "localhost:65535", "alice@localhost:1"] {
            assert!(display.parse::<X11Target>().is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn keeps_an_explicit_socket_path_even_before_the_socket_exists() {
        let path = "/tmp/shared-display/X1";
        assert_eq!(
            path.parse::<X11Target>().unwrap(),
            X11Target(X11Address::UnixSocket(PathBuf::from(path)))
        );
    }

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
