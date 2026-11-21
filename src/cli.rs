use std::ffi::OsStr;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::builder::{BoolishValueParser, TypedValueParser};
use clap::error::ErrorKind;
use clap::{Arg, ArgAction, Command, CommandFactory, Parser};

use crate::backend::{
    vnc::VncTarget,
    x11::{X11Authority, X11Connection},
};
use crate::terminal::{RenderOptions, Sampling, ScaleMode};
use crate::{RenderMode, ViewerOptions};

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub viewer: ViewerOptions,
}

#[derive(Clone)]
struct TargetParser;

impl TypedValueParser for TargetParser {
    type Value = ConnectionTarget;

    fn parse_ref(
        &self,
        cmd: &Command,
        _: Option<&Arg>,
        value: &OsStr,
    ) -> std::result::Result<Self::Value, clap::Error> {
        // The default value-error formatter repeats the invalid argument, which
        // could contain a password someone accidentally included in a VNC URL.
        let invalid =
            |message: String| clap::Error::raw(ErrorKind::ValueValidation, message).with_cmd(cmd);
        value
            .to_str()
            .ok_or_else(|| invalid("connection target must contain valid UTF-8".into()))?
            .parse::<ConnectionTarget>()
            .map_err(|error| invalid(error.to_string()))
    }
}

const TARGET_HELP: &str = "\
Targets:
  vnc://[USER@]HOST[:PORT]  VNC server (default port: 5901)
  x11://                   Use the current DISPLAY
  x11://[HOST]:DISPLAY[.N]  Use an explicit X display and optional screen N
  x11:///absolute/socket   Connect to a Unix socket by path

VNC uses VNC_PASSWORD or a hidden prompt. USER@ requests Apple Remote Desktop
account authentication. X11 uses XAUTHORITY (or ~/.Xauthority) by default.

Click [X] or press F12 to exit.";

#[derive(Parser)]
#[command(version, about = "View a VNC or X11 desktop in your terminal", after_help = TARGET_HELP)]
struct Cli {
    /// Connection URL; defaults to vnc://127.0.0.1:5901
    #[arg(value_name = "TARGET", value_parser = TargetParser)]
    target: Option<ConnectionTarget>,

    /// Select Kitty graphics or colored UTF-8 text
    #[arg(long, value_name = "MODE", value_enum, default_value_t)]
    render: RenderMode,

    /// Smooth the image when scaling; omit the value to enable
    #[arg(long, value_name = "0|1|true|false", action = ArgAction::Set, num_args = 0..=1, require_equals = true, default_missing_value = "true", value_parser = BoolishValueParser::new(), default_value_t = Sampling::default() == Sampling::Smooth)]
    smooth: bool,

    /// Stretch the desktop to fill the viewport; omit the value to enable
    #[arg(long, value_name = "0|1|true|false", action = ArgAction::Set, num_args = 0..=1, require_equals = true, default_missing_value = "true", value_parser = BoolishValueParser::new(), default_value_t = ScaleMode::default() == ScaleMode::Stretch)]
    fit: bool,

    /// X11 authorization file; overrides XAUTHORITY
    #[arg(long, value_name = "PATH")]
    xauthority: Option<PathBuf>,
}

impl Cli {
    fn into_viewer_args(self) -> std::result::Result<ViewerArgs, clap::Error> {
        let mut target = self.target.unwrap_or_default();
        if let Some(path) = self.xauthority {
            if path.as_os_str().is_empty() {
                return Err(Self::command()
                    .error(ErrorKind::InvalidValue, "--xauthority requires a file path"));
            }
            let ConnectionTarget::X11(connection) = &mut target else {
                return Err(Self::command().error(
                    ErrorKind::ArgumentConflict,
                    "--xauthority is only valid with an x11:// target",
                ));
            };
            connection.authority = X11Authority::File(path);
        }
        Ok(ViewerArgs {
            target,
            viewer: ViewerOptions {
                render: self.render,
                initial: RenderOptions {
                    scale: if self.fit {
                        ScaleMode::Stretch
                    } else {
                        ScaleMode::Proportional
                    },
                    sampling: if self.smooth {
                        Sampling::Smooth
                    } else {
                        Sampling::Nearest
                    },
                },
            },
        })
    }
}

pub fn parse_viewer_args() -> ViewerArgs {
    Cli::parse()
        .into_viewer_args()
        .unwrap_or_else(|error| error.exit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[&str]) -> std::result::Result<ViewerArgs, clap::Error> {
        Cli::try_parse_from(std::iter::once("gui-connect").chain(input.iter().copied()))?
            .into_viewer_args()
    }

    #[test]
    fn selects_a_typed_protocol_and_preserves_render_flags() {
        assert_eq!(parse(&[]).unwrap(), ViewerArgs::default());
        assert_eq!(
            parse(&["--render", "text", "vnc://alice@desktop.local:5902"]).unwrap(),
            ViewerArgs {
                target: ConnectionTarget::Vnc("alice@desktop.local:5902".parse().unwrap()),
                viewer: ViewerOptions {
                    render: RenderMode::Text,
                    ..ViewerOptions::default()
                },
            },
        );
        assert_eq!(
            parse(&["x11://", "--render=text"]).unwrap(),
            ViewerArgs {
                target: ConnectionTarget::X11(X11Connection::default()),
                viewer: ViewerOptions {
                    render: RenderMode::Text,
                    ..ViewerOptions::default()
                },
            },
        );
        assert_eq!(
            parse(&["x11://:99", "--render=kitty"]).unwrap(),
            ViewerArgs {
                target: ConnectionTarget::X11(X11Connection {
                    target: ":99".parse().unwrap(),
                    ..X11Connection::default()
                }),
                viewer: ViewerOptions::default(),
            },
        );
    }

    #[test]
    fn starts_smooth_and_proportional_by_default_but_accepts_boolean_overrides() {
        let defaults = parse(&["x11://"]).unwrap().viewer.initial;
        assert_eq!(defaults.scale, ScaleMode::Proportional);
        assert_eq!(defaults.sampling, Sampling::Smooth);

        for args in [
            &["x11://", "--smooth=0", "--fit=1"][..],
            &["--smooth=false", "--fit=true", "x11://"][..],
            &["--smooth=false", "--fit", "x11://"][..],
        ] {
            let parsed = parse(args).unwrap();
            assert_eq!(
                parsed.target,
                ConnectionTarget::X11(X11Connection::default())
            );
            assert_eq!(parsed.viewer.initial.scale, ScaleMode::Stretch);
            assert_eq!(parsed.viewer.initial.sampling, Sampling::Nearest);
        }
        for args in [
            &["--smooth=1", "--fit=0", "x11://"][..],
            &["--smooth=true", "--fit=false", "x11://"][..],
            &["--fit=false", "--smooth", "x11://"][..],
        ] {
            let parsed = parse(args).unwrap();
            assert_eq!(
                parsed.target,
                ConnectionTarget::X11(X11Connection::default())
            );
            assert_eq!(parsed.viewer.initial, defaults);
        }
    }

    #[test]
    fn generates_help_and_version() {
        let help = parse(&["--help"]).unwrap_err();
        assert_eq!(help.kind(), ErrorKind::DisplayHelp);
        let help = help.to_string();
        assert!(help.contains("--xauthority"));
        assert!(help.contains("--smooth"));
        assert!(help.contains("--fit"));
        assert!(help.contains("x11:///absolute/socket"));
        assert_eq!(
            parse(&["--version"]).unwrap_err().kind(),
            ErrorKind::DisplayVersion
        );
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
                ViewerArgs {
                    target: ConnectionTarget::X11(X11Connection {
                        authority: X11Authority::File(PathBuf::from("/tmp/session auth")),
                        ..X11Connection::default()
                    }),
                    viewer: ViewerOptions::default(),
                }
            );
        }
        assert!(parse(&["--xauthority", "file", "vnc://localhost"]).is_err());
        assert!(parse(&["--xauthority", "file"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn authority_paths_need_not_be_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let bytes = OsString::from_vec(b"auth-\xff".to_vec());
        let parsed = Cli::try_parse_from([
            OsString::from("gui-connect"),
            OsString::from("x11://"),
            OsString::from("--xauthority"),
            bytes.clone(),
        ])
        .unwrap()
        .into_viewer_args()
        .unwrap();
        let ConnectionTarget::X11(connection) = parsed.target else {
            panic!("expected X11")
        };
        assert_eq!(
            connection.authority,
            X11Authority::File(PathBuf::from(bytes))
        );
    }

    #[test]
    fn rejects_invalid_targets_and_flags() {
        for args in [
            &["--render"][..],
            &["--render=sixel"][..],
            &["--render=text", "--render=kitty"][..],
            &["--smooth=2"][..],
            &["--fit=maybe"][..],
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
