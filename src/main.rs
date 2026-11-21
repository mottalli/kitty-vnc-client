use anyhow::Result;
use gui_connect::backend::{vnc, x11};
use gui_connect::{ConnectionTarget, parse_viewer_args, require_terminal};

const HELP: &str = "\
Usage: gui-connect [OPTIONS] [TARGET]

Targets:
  vnc://[USER@]HOST[:PORT]    VNC server (default port: 5901)
  x11://                     Use the current DISPLAY
  x11://[HOST]:DISPLAY[.N]    Use an explicit X display and optional screen N
  x11:///absolute/socket     Connect to a Unix socket by path

If TARGET is omitted, connect to vnc://127.0.0.1:5901.
VNC uses VNC_PASSWORD or a hidden prompt. USER@ requests Apple Remote Desktop
account authentication. X11 uses XAUTHORITY (or ~/.Xauthority).

Options:
  --render MODE       kitty (default) or text (colored UTF-8)
  --xauthority PATH   X11 authorization file; overrides XAUTHORITY
  -h, --help          Show this help

Click Close or press F12 to exit.";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let Some(args) = parse_viewer_args(&args)? else {
        println!("{HELP}");
        return Ok(());
    };
    require_terminal()?;
    match args.target {
        ConnectionTarget::Vnc(target) => vnc::run(target, args.render).await,
        ConnectionTarget::X11(target) => x11::run(target, args.render).await,
    }
}
