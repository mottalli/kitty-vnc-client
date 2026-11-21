use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};

use crate::RenderMode;
use crate::backend::{
    vnc::VncTarget,
    x11::{X11Authority, X11Connection},
};

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectionTarget {
    Vnc(VncTarget),
    X11(X11Connection),
}

impl Default for ConnectionTarget {
    fn default() -> Self {
        Self::Vnc(VncTarget::default())
    }
}

impl FromStr for ConnectionTarget {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (scheme, address) = value
            .split_once("://")
            .context("expected a connection target starting with vnc:// or x11://")?;
        match scheme {
            "vnc" => Ok(Self::Vnc(address.parse()?)),
            "x11" if address.is_empty() => Ok(Self::X11(X11Connection::default())),
            "x11" => Ok(Self::X11(X11Connection {
                target: address.parse()?,
                authority: X11Authority::default(),
            })),
            _ => bail!("unsupported connection scheme; use vnc:// or x11://"),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ViewerArgs {
    pub target: ConnectionTarget,
    pub render: RenderMode,
}

pub fn parse_viewer_args(args: &[String]) -> Result<Option<ViewerArgs>> {
    let mut target = None;
    let mut render = None;
    let mut authority = None;
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        let authority_path = match arg {
            "--xauthority" => Some(
                args.next()
                    .filter(|path| !path.starts_with('-'))
                    .context("--xauthority requires a file path")?,
            ),
            arg if arg.starts_with("--xauthority=") => Some(&arg["--xauthority=".len()..]),
            _ => None,
        };
        if let Some(path) = authority_path {
            ensure!(!path.is_empty(), "--xauthority requires a file path");
            ensure!(
                authority
                    .replace(X11Authority::File(PathBuf::from(path)))
                    .is_none(),
                "specify --xauthority only once"
            );
            continue;
        }
        let value = match arg {
            "-h" | "--help" => return Ok(None),
            "--render" => args.next().context("--render requires kitty or text")?,
            arg if arg.starts_with("--render=") => &arg["--render=".len()..],
            arg if arg.starts_with('-') => bail!("unknown option {arg} (see --help)"),
            address => {
                ensure!(
                    target.is_none(),
                    "expected at most one connection target (see --help)"
                );
                target = Some(address);
                continue;
            }
        };
        ensure!(render.is_none(), "specify --render only once");
        render = Some(match value {
            "kitty" => RenderMode::Kitty,
            "text" => RenderMode::Text,
            _ => bail!("invalid render mode {value:?}; use kitty or text"),
        });
    }
    let mut target = target.map(str::parse).transpose()?.unwrap_or_default();
    if let Some(authority) = authority {
        match &mut target {
            ConnectionTarget::X11(connection) => connection.authority = authority,
            ConnectionTarget::Vnc(_) => bail!("--xauthority is only valid with an x11:// target"),
        }
    }
    Ok(Some(ViewerArgs {
        target,
        render: render.unwrap_or_default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[&str]) -> Result<Option<ViewerArgs>> {
        parse_viewer_args(&input.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn selects_a_typed_protocol_and_preserves_render_flags() {
        assert_eq!(parse(&[]).unwrap(), Some(ViewerArgs::default()));
        assert_eq!(
            parse(&["--render", "text", "vnc://alice@desktop.local:5902"]).unwrap(),
            Some(ViewerArgs {
                target: ConnectionTarget::Vnc("alice@desktop.local:5902".parse().unwrap()),
                render: RenderMode::Text,
            }),
        );
        assert_eq!(
            parse(&["x11://", "--render=text"]).unwrap(),
            Some(ViewerArgs {
                target: ConnectionTarget::X11(X11Connection::default()),
                render: RenderMode::Text,
            }),
        );
        assert_eq!(
            parse(&["x11://:99", "--render=kitty"]).unwrap(),
            Some(ViewerArgs {
                target: ConnectionTarget::X11(X11Connection {
                    target: ":99".parse().unwrap(),
                    ..X11Connection::default()
                }),
                render: RenderMode::Kitty,
            }),
        );
        assert!(parse(&["--help"]).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_absolute_x11_socket_path() {
        assert_eq!(
            "x11:///tmp/display/X1".parse::<ConnectionTarget>().unwrap(),
            ConnectionTarget::X11(X11Connection {
                target: "/tmp/display/X1".parse().unwrap(),
                ..X11Connection::default()
            }),
        );
    }

    #[test]
    fn parses_explicit_xauthority_only_for_x11() {
        for args in [
            &["--xauthority", "/tmp/session auth", "x11://"][..],
            &["x11://", "--xauthority=/tmp/session auth"][..],
        ] {
            assert_eq!(
                parse(args).unwrap(),
                Some(ViewerArgs {
                    target: ConnectionTarget::X11(X11Connection {
                        authority: X11Authority::File(PathBuf::from("/tmp/session auth")),
                        ..X11Connection::default()
                    }),
                    render: RenderMode::Kitty,
                })
            );
        }
        assert!(parse(&["--xauthority", "file", "vnc://localhost"]).is_err());
        assert!(parse(&["--xauthority", "file"]).is_err());
    }

    #[test]
    fn rejects_invalid_targets_and_flags() {
        for args in [
            &["--render"][..],
            &["--render=sixel"][..],
            &["--render=text", "--render=kitty"][..],
            &["x11://:98", "x11://:99"][..],
            &["--server", "vnc://localhost"][..],
            &["localhost:5901"][..],
            &["rdp://localhost"][..],
            &["vnc://"][..],
            &["vnc://alice:secret@localhost"][..],
            &["x11://", "--xauthority"][..],
            &["x11://", "--xauthority="][..],
            &["x11://", "--xauthority=a", "--xauthority=b"][..],
        ] {
            let error = parse(args).unwrap_err();
            assert!(!error.to_string().contains("secret"));
        }
    }
}
