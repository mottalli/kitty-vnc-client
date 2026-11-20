# VNC viewer for Kitty-compatible terminals

Run inside a Rex pane:

```sh
cargo run --release
```

The default server is `127.0.0.1:5901`. To use another address, run
`cargo run --release -- HOST:PORT`. Enter the VNC password at the hidden prompt.
The desktop scales up or down to fit the pane without stretching. Mouse movement, clicks, drags,
scrolling, and common keyboard shortcuts go to the remote desktop. **F12 exits
locally.**

This proof of concept uses standard VNC password authentication and does not
encrypt the session. Keep it on loopback, or use an SSH tunnel for a remote
server. It redraws the whole Kitty image when the framebuffer changes. Mouse
position falls back to character-cell resolution if the pane's PTY doesn't
report its pixel size. Clipboard sync and held-key combinations are not
implemented.
