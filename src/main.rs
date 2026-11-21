use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use crossterm::event::{EventStream, KeyEvent};
use futures_util::StreamExt;
use terminal_vnc_kitty::{
    Frame, KEY_MODIFIERS, PointerUpdate, RenderMode, Viewer, ViewerEvent, key_tap,
    parse_viewer_args,
};
use tokio::net::TcpStream;
use tokio::time::{self, MissedTickBehavior};
use vnc::{PixelFormat, VncClient, VncConnector, VncEncoding, VncError, VncEvent, X11Event};

const DEFAULT_SERVER: &str = "127.0.0.1:5901";

fn server_address(target: Option<&str>) -> Result<&str> {
    let address = target.unwrap_or(DEFAULT_SERVER);
    ensure!(
        address.rsplit_once(':').is_some_and(|(host, port)| {
            !host.is_empty()
                && !host.starts_with('-')
                && (!host.contains(':') || (host.starts_with('[') && host.ends_with(']')))
                && port.parse::<u16>().is_ok_and(|port| port != 0)
        }),
        "invalid server address; expected HOST:PORT (use [IPv6]:PORT for IPv6)"
    );
    Ok(address)
}

async fn send_pointer(client: &VncClient, update: PointerUpdate) -> Result<(), VncError> {
    let PointerUpdate {
        x,
        y,
        buttons,
        wheel,
    } = update;
    client
        .input(X11Event::PointerEvent((x, y, buttons | wheel).into()))
        .await?;
    if wheel != 0 {
        client
            .input(X11Event::PointerEvent((x, y, buttons).into()))
            .await?;
    }
    Ok(())
}

async fn send_key(client: &VncClient, key: KeyEvent) -> Result<(), VncError> {
    let Some((symbol, modifiers)) = key_tap(key) else {
        return Ok(());
    };
    for (flag, key) in KEY_MODIFIERS {
        if modifiers.contains(flag) {
            client.input(X11Event::KeyEvent((key, true).into())).await?;
        }
    }
    for down in [true, false] {
        client
            .input(X11Event::KeyEvent((symbol, down).into()))
            .await?;
    }
    for (flag, key) in KEY_MODIFIERS.into_iter().rev() {
        if modifiers.contains(flag) {
            client
                .input(X11Event::KeyEvent((key, false).into()))
                .await?;
        }
    }
    Ok(())
}

async fn view(client: &VncClient, render: RenderMode) -> Result<()> {
    let mut viewer = Viewer::open(render)?;
    let mut events = EventStream::new();
    let mut tick = time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tick.tick().await; // The library already requests a full initial framebuffer.
    let mut frame = None::<Frame>;
    let mut dirty = false;

    loop {
        tokio::select! {
            event = events.next() => match viewer.handle(event.context("terminal input closed")??) {
                ViewerEvent::Quit => {
                    if let Some(update) = viewer.reset_pointer() {
                        send_pointer(client, update).await?;
                    }
                    return Ok(());
                },
                ViewerEvent::Key(key) => send_key(client, key).await?,
                ViewerEvent::Pointer(update) => send_pointer(client, update).await?,
                ViewerEvent::None => {},
            },
            event = client.recv_event() => match event.context("VNC connection closed")? {
                VncEvent::SetResolution(size) => {
                    if let Some(update) = viewer.reset_pointer() {
                        send_pointer(client, update).await?;
                    }
                    frame = Some(Frame::new(size.width, size.height));
                    dirty = true;
                }
                VncEvent::RawImage(rect, pixels) => {
                    frame.as_mut().context("pixels arrived before desktop size")?
                        .update((rect.x, rect.y, rect.width, rect.height), &pixels)?;
                    dirty = true;
                }
                VncEvent::Error(error) => bail!("VNC error: {error}"),
                _ => {}
            },
            _ = tick.tick() => {
                client.input(X11Event::Refresh).await?;
                if let Some(update) = viewer.render(frame.as_ref(), dirty)? {
                    send_pointer(client, update).await?;
                }
                dirty = false;
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let Some(args) = parse_viewer_args(&args)? else {
        println!(
            "Usage: {} [--render kitty|text] [HOST:PORT]\n\n  HOST:PORT         VNC server (default: {DEFAULT_SERVER})\n  --render MODE     kitty (default) or text (colored UTF-8)\n  -h, --help        Show this help\n\nUses VNC_PASSWORD if set, otherwise prompts.\nClick Close or press F12 to exit.",
            env!("CARGO_PKG_NAME"),
        );
        return Ok(());
    };
    let address = server_address(args.target)?;
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "run this directly in an interactive terminal pane"
    );

    let password = match std::env::var("VNC_PASSWORD") {
        Ok(password) => password,
        Err(std::env::VarError::NotPresent) => rpassword::prompt_password("VNC password: ")?,
        Err(_) => bail!("VNC_PASSWORD must contain valid UTF-8"),
    };
    let client = time::timeout(Duration::from_secs(15), async {
        let stream = TcpStream::connect(address)
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

    let result = view(&client, args.render).await;
    let _ = time::timeout(Duration::from_secs(1), client.close()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_address_and_default() {
        assert_eq!(server_address(None).unwrap(), "127.0.0.1:5901");
        for addr in ["desktop.local:5902", "[::1]:5903"] {
            assert_eq!(server_address(Some(addr)).unwrap(), addr);
        }
    }

    #[test]
    fn rejects_invalid_server_arguments() {
        for addr in ["localhost", "localhost:65536", "::1:5901"] {
            assert!(server_address(Some(addr)).is_err());
        }
    }
}
