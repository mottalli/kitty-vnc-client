use anyhow::Result;
use gui_connect::backend::{vnc, x11};
use gui_connect::{ConnectionTarget, parse_viewer_args, require_terminal};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = parse_viewer_args();
    require_terminal()?;
    match args.target {
        ConnectionTarget::Vnc(target) => vnc::run(target, args.render).await,
        ConnectionTarget::X11(target) => x11::run(target, args.render).await,
    }
}
