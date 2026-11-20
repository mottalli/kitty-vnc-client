# VNC viewer for Kitty-compatible terminals

Run inside a Rex pane:

```sh
cargo run --release
```

The default server is `127.0.0.1:5901`. Pass `HOST:PORT` as the only argument
to connect to a different server:

```sh
cargo run --release -- desktop.local:5902
```

Put IPv6 addresses in brackets, for example `cargo run --release -- '[::1]:5901'`.
The viewer uses `VNC_PASSWORD` if set; otherwise it prompts for the password
without echoing it.

The desktop is centered in a border that fills the pane and automatically
rescales when the pane is resized. Click the **Fit / Fill** button to switch
modes: **Fit** preserves proportions; **Fill** stretches the full desktop
to cover the inside of the frame without cropping. In both viewers, click
**Smooth** to switch between filtered scaling (the default) and the original
nearest-neighbor scaling. Neither mode alters the image at native size.
Mouse movement, clicks,
drags, scrolling, and common keyboard shortcuts go to the remote desktop.
**Click Close or press F12** to exit locally.

This proof of concept uses standard VNC password authentication and does not
encrypt the session. Keep it on loopback, or use an SSH tunnel for a remote
server. It redraws the whole Kitty image when the framebuffer changes. Mouse
position falls back to character-cell resolution if the pane's PTY doesn't
report its pixel size. Clipboard sync and held-key combinations are not
implemented.

## Direct X11 viewer (experimental)

From the shell started by `xvfb-run`, run an X11 app and launch the separate
viewer in the same Rex pane:

```sh
xclock &
cargo run --release --bin kitty-x11
```

It connects directly to the X server in `$DISPLAY` (e.g. `:99`) using your
existing X authorization. You can also pass a display explicitly with
`cargo run --release --bin kitty-x11 -- :99`; keep the matching `XAUTHORITY`.
On Unix, you can also pass the absolute path to a mounted X11 socket instead
of a display name. In that case the viewer uses screen 0 and looks up display
0 in the authorization file.
No VNC server or password is needed. The viewer captures the entire X screen
and forwards mouse and basic keyboard input via XTEST. Click **Close** or
press **F12** to exit the viewer without stopping Xvfb.
