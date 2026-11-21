use std::fmt;
use std::future::ready;
use std::net::{Ipv4Addr, SocketAddr};
use std::num::NonZeroU16;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tokio::{net::TcpStream, time};
use vnc::{PixelFormat, VncClient, VncConnector, VncEncoding, VncError, VncEvent, X11Event};

use crate::input::{
    DesktopInput, KEY_MODIFIERS, KeyTap, PointerButton, PointerUpdate, ScrollDirection,
};
use crate::session::{self, DesktopBackend, DesktopUpdate};
use crate::{Frame, FrameRegion, RenderMode};

const DEFAULT_PORT: NonZeroU16 = NonZeroU16::new(5901).unwrap();

#[derive(Debug, PartialEq, Eq)]
struct VncEndpoint(VncAddress);

#[derive(Debug, PartialEq, Eq)]
enum VncAddress {
    Ip(SocketAddr),
    Named { host: String, port: NonZeroU16 },
}

impl Default for VncEndpoint {
    fn default() -> Self {
        Self(VncAddress::Ip(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            DEFAULT_PORT.get(),
        ))))
    }
}

impl FromStr for VncEndpoint {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        const EXPECTED: &str = "invalid VNC address; expected HOST[:PORT] (use [IPv6] for IPv6)";
        if let Ok(address) = value.parse::<SocketAddr>() {
            ensure!(address.port() != 0, "{EXPECTED}");
            return Ok(Self(VncAddress::Ip(address)));
        }
        if value.starts_with('[') && value.ends_with(']') {
            return format!("{value}:{DEFAULT_PORT}")
                .parse::<SocketAddr>()
                .map(|address| Self(VncAddress::Ip(address)))
                .context(EXPECTED);
        }
        let (host, port) = match value.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().context(EXPECTED)?),
            None => (value, DEFAULT_PORT),
        };
        ensure!(
            !host.is_empty()
                && !host.starts_with('-')
                && !host.contains([':', '[', ']', '@', '/', '?', '#'])
                && !host.contains(|c: char| c.is_whitespace() || c.is_control()),
            "{EXPECTED}"
        );
        if let Ok(ip) = host.parse::<Ipv4Addr>() {
            return Ok(Self(VncAddress::Ip(SocketAddr::from((ip, port.get())))));
        }
        Ok(Self(VncAddress::Named {
            host: host.to_owned(),
            port,
        }))
    }
}

impl fmt::Display for VncEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            VncAddress::Ip(address) => address.fmt(f),
            VncAddress::Named { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

impl VncEndpoint {
    async fn connect(&self) -> Result<TcpStream> {
        match &self.0 {
            VncAddress::Ip(address) => TcpStream::connect(address).await,
            VncAddress::Named { host, port } => {
                TcpStream::connect((host.as_str(), port.get())).await
            }
        }
        .with_context(|| format!("could not connect to {self}"))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct VncUsername(String);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct VncTarget {
    endpoint: VncEndpoint,
    username: Option<VncUsername>,
}

impl FromStr for VncTarget {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (username, endpoint) = match value.split_once('@') {
            Some((user, host)) => {
                ensure!(
                    !user.is_empty()
                        && !user.contains([':', '/', '?', '#', '[', ']'])
                        && !user.contains(|c: char| c.is_whitespace() || c.is_control()),
                    "invalid VNC username; use USER@HOST and supply the password via VNC_PASSWORD or the prompt"
                );
                (Some(VncUsername(user.into())), host)
            }
            None => (None, value),
        };
        Ok(Self {
            endpoint: endpoint.parse()?,
            username,
        })
    }
}

// Keep credentials separate from parsed targets and never format them in diagnostics.
struct VncPassword(String);

impl VncPassword {
    fn from_env_or_prompt() -> Result<Self> {
        match std::env::var("VNC_PASSWORD") {
            Ok(password) => Ok(Self(password)),
            Err(std::env::VarError::NotPresent) => {
                Ok(Self(rpassword::prompt_password("VNC password: ")?))
            }
            Err(_) => bail!("VNC_PASSWORD must contain valid UTF-8"),
        }
    }
}

enum Authentication {
    Password(VncPassword),
    AppleRemoteDesktop {
        username: VncUsername,
        password: VncPassword,
    },
}

struct VncConnection {
    endpoint: VncEndpoint,
    authentication: Authentication,
}

pub async fn run(VncTarget { endpoint, username }: VncTarget, render: RenderMode) -> Result<()> {
    let password = VncPassword::from_env_or_prompt()?;
    let authentication = match username {
        Some(username) => Authentication::AppleRemoteDesktop { username, password },
        None => Authentication::Password(password),
    };
    let connection = VncConnection {
        endpoint,
        authentication,
    };
    let desktop = time::timeout(Duration::from_secs(15), VncDesktop::connect(connection))
        .await
        .context("timed out connecting to the VNC server")??;
    session::run(desktop, render).await
}

struct VncDesktop {
    client: VncClient,
    frame: Option<Frame>,
}

impl VncDesktop {
    async fn connect(
        VncConnection {
            endpoint,
            authentication,
        }: VncConnection,
    ) -> Result<Self> {
        let stream = endpoint.connect().await?;
        let connector = VncConnector::new(stream)
            .allow_shared(true)
            .set_pixel_format(PixelFormat::rgba())
            .add_encoding(VncEncoding::Raw)
            .add_encoding(VncEncoding::DesktopSizePseudo);
        let connector = match authentication {
            Authentication::Password(password) => connector.set_auth_method(ready(Ok(password.0))),
            Authentication::AppleRemoteDesktop { username, password } => {
                connector.set_ard_credentials(username.0, password.0)
            }
        };
        let client = connector
            .build()?
            .try_start()
            .await
            .context("VNC handshake failed")?
            .finish()?;
        Ok(Self {
            client,
            frame: None,
        })
    }

    async fn send_pointer(&self, update: PointerUpdate) -> Result<(), VncError> {
        let buttons = PointerButton::ALL
            .into_iter()
            .filter(|button| update.buttons.contains(*button))
            .fold(0, |mask, button| {
                mask | match button {
                    PointerButton::Left => 1,
                    PointerButton::Middle => 2,
                    PointerButton::Right => 4,
                }
            });
        let wheel = match update.scroll {
            Some(ScrollDirection::Up) => 8,
            Some(ScrollDirection::Down) => 16,
            Some(ScrollDirection::Left) => 32,
            Some(ScrollDirection::Right) => 64,
            None => 0,
        };
        self.client
            .input(X11Event::PointerEvent(
                (update.x, update.y, buttons | wheel).into(),
            ))
            .await?;
        if wheel != 0 {
            self.client
                .input(X11Event::PointerEvent((update.x, update.y, buttons).into()))
                .await?;
        }
        Ok(())
    }

    async fn send_key(&self, key: KeyTap) -> Result<(), VncError> {
        for (flag, symbol) in KEY_MODIFIERS {
            if key.modifiers.contains(flag) {
                self.client
                    .input(X11Event::KeyEvent((symbol, true).into()))
                    .await?;
            }
        }
        for down in [true, false] {
            self.client
                .input(X11Event::KeyEvent((key.keysym, down).into()))
                .await?;
        }
        for (flag, symbol) in KEY_MODIFIERS.into_iter().rev() {
            if key.modifiers.contains(flag) {
                self.client
                    .input(X11Event::KeyEvent((symbol, false).into()))
                    .await?;
            }
        }
        Ok(())
    }
}

impl DesktopBackend for VncDesktop {
    fn frame(&self) -> Option<&Frame> {
        self.frame.as_ref()
    }

    async fn next_update(&mut self) -> Result<DesktopUpdate> {
        // vnc-rs uses a cancellation-safe receive on its decoded-event channel.
        match self
            .client
            .recv_event()
            .await
            .context("VNC connection closed")?
        {
            VncEvent::SetResolution(size) => {
                self.frame = Some(Frame::new(size.width, size.height));
                Ok(DesktopUpdate::Resized)
            }
            VncEvent::RawImage(rect, pixels) => {
                let region = FrameRegion {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                };
                self.frame
                    .as_mut()
                    .context("pixels arrived before desktop size")?
                    .update(region, &pixels)?;
                Ok(DesktopUpdate::FrameChanged)
            }
            VncEvent::Error(error) => bail!("VNC error: {error}"),
            _ => Ok(DesktopUpdate::Unchanged),
        }
    }

    async fn refresh(&mut self) -> Result<DesktopUpdate> {
        self.client.input(X11Event::Refresh).await?;
        Ok(DesktopUpdate::Unchanged)
    }

    async fn send_input(&mut self, input: DesktopInput) -> Result<()> {
        match input {
            DesktopInput::Key(key) => self.send_key(key).await?,
            DesktopInput::Pointer(pointer) => self.send_pointer(pointer).await?,
        }
        Ok(())
    }

    async fn close(&mut self) {
        let _ = self.client.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_address_and_default() {
        assert_eq!(VncEndpoint::default().to_string(), "127.0.0.1:5901");
        for addr in ["127.0.0.1:5901", "[::1]:5903"] {
            assert_eq!(
                addr.parse::<VncEndpoint>().unwrap().0,
                VncAddress::Ip(addr.parse().unwrap())
            );
        }
        assert_eq!(
            "desktop.local:5902".parse::<VncEndpoint>().unwrap().0,
            VncAddress::Named {
                host: "desktop.local".into(),
                port: NonZeroU16::new(5902).unwrap()
            },
        );
        for (address, with_port) in [
            ("127.0.0.1", "127.0.0.1:5901"),
            ("[::1]", "[::1]:5901"),
            ("desktop.local", "desktop.local:5901"),
        ] {
            assert_eq!(
                address.parse::<VncEndpoint>().unwrap().to_string(),
                with_port
            );
        }
    }

    #[test]
    fn rejects_invalid_endpoints() {
        for addr in [
            "",
            "localhost:65536",
            "::1:5901",
            "localhost:0",
            "[::1]:0",
            "localhost/screen",
        ] {
            assert!(addr.parse::<VncEndpoint>().is_err(), "{addr}");
        }
    }

    #[test]
    fn parses_usernames_but_does_not_accept_passwords_in_targets() {
        let target: VncTarget = "alice@[::1]".parse().unwrap();
        assert_eq!(target.username, Some(VncUsername("alice".into())));
        assert_eq!(target.endpoint.to_string(), "[::1]:5901");
        assert!(
            "desktop.local"
                .parse::<VncTarget>()
                .unwrap()
                .username
                .is_none()
        );
        for value in ["@host", "a@b@host", "a:secret@host"] {
            let error = value.parse::<VncTarget>().unwrap_err();
            assert!(!error.to_string().contains("secret"));
        }
    }
}
