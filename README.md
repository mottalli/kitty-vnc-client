# VNC viewer for Kitty-compatible terminals

Run inside a Rex pane:

```sh
cargo run --release
```

The default server is `127.0.0.1:5901`. To use another address, run
`cargo run --release -- HOST:PORT`. The viewer uses `VNC_PASSWORD` if set;
otherwise it prompts for the password without echoing it.

The desktop is centered in a border that fills the pane and automatically
rescales when the pane is resized. Click the **Fit / Fill** button to switch
modes: **Fit** preserves proportions; **Fill** stretches the full desktop
to cover the inside of the frame without cropping. Mouse movement, clicks,
drags, scrolling, and common keyboard shortcuts go to the remote desktop.
**Click Close or press F12** to exit locally.

This proof of concept uses standard VNC password authentication and does not
encrypt the session. Keep it on loopback, or use an SSH tunnel for a remote
server. It redraws the whole Kitty image when the framebuffer changes. Mouse
position falls back to character-cell resolution if the pane's PTY doesn't
report its pixel size. Clipboard sync and held-key combinations are not
implemented.
