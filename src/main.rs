mod input;
mod terminal;

use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::time::{self, MissedTickBehavior};
use vnc::{PixelFormat, Rect, VncClient, VncConnector, VncEncoding, VncEvent, X11Event};

use crate::input::Pointer;
use crate::terminal::{Terminal, Viewport};

struct Frame {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Frame {
    fn new(width: u16, height: u16) -> Self {
        let (width, height) = (usize::from(width), usize::from(height));
        Self {
            width,
            height,
            rgba: vec![0; width * height * 4],
        }
    }

    fn update(&mut self, rect: Rect, pixels: &[u8]) -> Result<()> {
        let [x, y, width, height] = [rect.x, rect.y, rect.width, rect.height].map(usize::from);
        ensure!(
            x + width <= self.width
                && y + height <= self.height
                && pixels.len() == width * height * 4,
            "invalid VNC framebuffer rectangle"
        );
        for row in 0..height {
            let src = row * width * 4;
            let dst = ((y + row) * self.width + x) * 4;
            self.rgba[dst..dst + width * 4].copy_from_slice(&pixels[src..src + width * 4]);
        }
        Ok(())
    }

    fn scaled_rgb(&self, width: usize, height: usize) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            let row = (y * self.height / height) * self.width;
            for x in 0..width {
                let offset = (row + x * self.width / width) * 4;
                // The fourth byte of a VNC 32-bit true-color pixel is padding.
                rgb.extend_from_slice(&self.rgba[offset..offset + 3]);
            }
        }
        rgb
    }
}

async fn view(client: &VncClient, terminal: &mut Terminal) -> Result<()> {
    let mut events = EventStream::new();
    let mut pointer = Pointer::default();
    let mut tick = time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tick.tick().await; // The library already requests a full initial framebuffer.
    let mut frame = None::<Frame>;
    let mut viewport = None::<Viewport>;
    let mut dirty = false;

    loop {
        tokio::select! {
            event = events.next() => match event.context("terminal input closed")?? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key.code == KeyCode::F(12) {
                        pointer.release(client).await?;
                        return Ok(());
                    }
                    input::send_key(client, key).await?;
                }
                Event::Mouse(mouse) => {
                    if let Some(viewport) = viewport {
                        pointer.send(client, mouse, viewport).await?;
                    }
                }
                Event::Resize(..) => dirty = frame.is_some(),
                _ => {}
            },
            event = client.recv_event() => match event.context("VNC connection closed")? {
                VncEvent::SetResolution(size) => {
                    frame = Some(Frame::new(size.width, size.height));
                    viewport = None;
                    dirty = true;
                }
                VncEvent::RawImage(rect, pixels) => {
                    frame.as_mut().context("pixels arrived before desktop size")?
                        .update(rect, &pixels)?;
                    dirty = true;
                }
                VncEvent::Error(error) => bail!("VNC error: {error}"),
                _ => {}
            },
            _ = tick.tick() => {
                client.input(X11Event::Refresh).await?;
                if dirty
                    && let Some(frame) = &frame
                    && let Some(current) = terminal.viewport(frame.width, frame.height)?
                {
                    terminal.draw(current, frame)?;
                    viewport = Some(current);
                    dirty = false;
                }
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "127.0.0.1:5901".into());
    if matches!(address.as_str(), "-h" | "--help") {
        println!(
            "Usage: terminal-x11-kitty [HOST:PORT]\nDefault: 127.0.0.1:5901\nF12 exits the viewer."
        );
        return Ok(());
    }
    ensure!(
        args.next().is_none(),
        "usage: terminal-x11-kitty [HOST:PORT]"
    );
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "run this directly in a Kitty graphics-compatible terminal pane"
    );

    let password = rpassword::prompt_password("VNC password: ")?;
    let client = time::timeout(Duration::from_secs(15), async {
        let stream = TcpStream::connect(&address)
            .await
            .with_context(|| format!("could not connect to {address}"))?;
        let client = VncConnector::new(stream)
            .set_auth_method(async move { Ok(password) })
            .allow_shared(true)
            .set_pixel_format(PixelFormat::rgba())
            .add_encoding(VncEncoding::Raw)
            .add_encoding(VncEncoding::DesktopSizePseudo)
            .build()?
            .try_start()
            .await
            .context("VNC handshake failed")?
            .finish()?;
        Ok::<_, anyhow::Error>(client)
    })
    .await
    .context("timed out connecting to the VNC server")??;

    let result = {
        let mut terminal = Terminal::open()?;
        view(&client, &mut terminal).await
    };
    let _ = time::timeout(Duration::from_secs(1), client.close()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_rectangles_and_ignores_pixel_padding() {
        let mut frame = Frame::new(2, 2);
        let rect = Rect {
            x: 1,
            y: 0,
            width: 1,
            height: 2,
        };
        frame.update(rect, &[1, 2, 3, 0, 4, 5, 6, 255]).unwrap();
        assert_eq!(frame.scaled_rgb(2, 2), [0, 0, 0, 1, 2, 3, 0, 0, 0, 4, 5, 6]);
        assert!(frame.update(rect, &[1, 2, 3]).is_err());
    }
}
