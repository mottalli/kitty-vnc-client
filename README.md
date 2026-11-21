# gui-connect

A terminal viewer for VNC desktops and direct X11 displays. Run it inside a
Rex pane, or install the `gui-connect` binary with `cargo install --path .`.

```sh
cargo run --release -- vnc://desktop.local:5902
cargo run --release -- x11://
```

## VNC connections

Use `vnc://[user@]host[:port]`. The default port is `5901`; omitting the target
entirely connects to `vnc://127.0.0.1:5901`.

```sh
gui-connect vnc://desktop.local
gui-connect 'vnc://[::1]:5901'
gui-connect vnc://alice@mac.local:5900
```

Include `user@` to request Apple Remote Desktop account authentication.
Standard VNC password authentication does not use a username. Set
`VNC_PASSWORD` or enter the password at the hidden prompt; passwords in the
connection URL are not accepted.

## Direct X11 connections

X11 targets use X display notation, **not** `host:port`:

| Target | Meaning |
| --- | --- |
| `x11://` | Use the display set in `$DISPLAY`. |
| `x11://:99` | Connect to display `99` on the local machine. |
| `x11://host.example:1.0` | Connect to display `1`, screen `0`, on a host whose X server listens for TCP connections. X11 display `1` uses TCP port `6001`. |
| `x11:///path/to/socket/X1` | Connect directly to an absolute Unix socket path, such as a socket shared from a container. |

The `.screen` suffix is optional and defaults to `0`. X11 uses an
authorization file, not a username or VNC password. Pass `--xauthority PATH`
to select a file explicitly; otherwise the viewer uses `XAUTHORITY` or
`~/.Xauthority`. For an absolute socket path, the viewer uses screen `0` and
looks for display `0` in that file. The path can be passed on the command
line, but keep the cookie inside the file private.

```sh
gui-connect --xauthority "$auth" "x11://$bridge/socket/X1"
```

For example, from a shell started by `xvfb-run`:

```sh
xclock &
cargo run --release -- x11://
```

The viewer captures the entire X screen and forwards mouse and basic
keyboard input via XTEST. Closing it doesn't stop Xvfb.

## Viewer controls

The desktop is centered in a border that fills the pane and automatically
rescales when the pane is resized. Click the **Fit / Fill** button to switch
modes: **Fit** preserves proportions; **Fill** stretches the full desktop
to cover the inside of the frame without cropping. Click
**Smooth** to switch between filtered scaling (the default) and the original
nearest-neighbor scaling. Neither mode alters the image at native size.
Mouse movement, clicks,
drags, scrolling, and common keyboard shortcuts go to the remote desktop.
**Click Close or press F12** to exit locally.

## Text rendering

Kitty graphics are the default for both backends. To use colored UTF-8
half-blocks instead, pass `--render text`:

```sh
cargo run --release -- --render text vnc://localhost
cargo run --release -- --render text x11://:99
```

Text mode uses 24-bit terminal colors and the `▀` character for two image
samples per cell. It doesn't require Kitty graphics support. Fit / Fill,
smoothing, and mouse and keyboard input still work, but mouse coordinates
are limited to cells and small desktop text may be hard to read.

## Limitations

VNC and X11 connections do not encrypt the desktop session. Keep them local
or use a trusted tunnel for a remote server. The Kitty renderer redraws the
whole image when the framebuffer changes. Mouse
position falls back to character-cell resolution if the pane's PTY doesn't
report its pixel size. Clipboard sync and held-key combinations are not
implemented.

## Code layout

Both backends share the terminal UI in `src/terminal.rs`. It selects a
`Renderer` once at startup and manages the terminal, toolbar, and redraws.
`src/terminal/kitty.rs` handles image uploads and placement;
`src/terminal/text.rs` handles half-block colors and cell diffs.
`src/terminal/geometry.rs` distinguishes raster sizes, cell rectangles, and
pixel rectangles so input maps to the actual area each renderer drew.

`src/cli.rs` parses typed connection targets; the binary selects a backend.
The VNC and X11 backends own their connections, framebuffers, and protocol
input. `src/session.rs` runs the shared refresh, input, and shutdown loop;
`src/viewer.rs` manages viewer state and local controls.

Run `cargo test` to check the renderers and shared session. The session tests
use scripted events, a fake desktop backend, and an in-memory terminal to
exercise redraws, input routing, and cleanup without Rex, Xvfb, or VNC.
