use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::io::Write;
use std::rc::Rc;
use std::task::Poll;

use anyhow::{bail, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use crossterm::terminal::WindowSize;

use super::*;
use crate::RenderMode;
use crate::input::PointerButton;
use crate::terminal::{RenderOptions, Sampling, ScaleMode};

enum Step {
    Input(Event),
    Resize { cols: u16, rows: u16 },
    Refresh,
    FailDraw,
    Disconnect,
}

#[derive(Debug, PartialEq, Eq)]
enum Observed {
    RawMode(bool),
    Refresh,
    Key(u32),
    Pointer { x: u16, y: u16, left_down: bool },
    Close,
}

struct Script {
    steps: VecDeque<Step>,
    observed: Vec<Observed>,
    output: Vec<u8>,
    cols: u16,
    rows: u16,
    fail_write: bool,
}

type Trace = Rc<RefCell<Script>>;

struct MemoryTerminal(Trace);

impl Write for MemoryTerminal {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut script = self.0.borrow_mut();
        if std::mem::take(&mut script.fail_write) {
            return Err(io::Error::other("scripted draw failure"));
        }
        script.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TerminalIo for MemoryTerminal {
    fn writer(&mut self) -> impl Write {
        self
    }

    fn window_size(&self) -> io::Result<WindowSize> {
        let script = self.0.borrow();
        Ok(WindowSize {
            columns: script.cols,
            rows: script.rows,
            width: script.cols * 8,
            height: script.rows * 16,
        })
    }

    fn set_raw_mode(&mut self, enabled: bool) -> io::Result<()> {
        self.0
            .borrow_mut()
            .observed
            .push(Observed::RawMode(enabled));
        Ok(())
    }
}

struct FakeDesktop {
    trace: Trace,
    frame: Frame,
    fresh_frame: bool,
    connected: bool,
}

impl DesktopBackend for FakeDesktop {
    fn frame_title(&self) -> &'static str {
        "Test Driver"
    }

    fn frame(&self) -> Option<&Frame> {
        Some(&self.frame)
    }

    async fn next_update(&mut self) -> Result<DesktopUpdate> {
        poll_fn(|_| {
            let mut script = self.trace.borrow_mut();
            if matches!(script.steps.front(), Some(Step::Disconnect)) {
                script.steps.pop_front();
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        self.connected = false;
        bail!("scripted disconnect")
    }

    async fn refresh(&mut self) -> Result<DesktopUpdate> {
        self.trace.borrow_mut().observed.push(Observed::Refresh);
        // No desktop activity after the first capture.
        Ok(if std::mem::take(&mut self.fresh_frame) {
            DesktopUpdate::FrameChanged
        } else {
            DesktopUpdate::Unchanged
        })
    }

    async fn send_input(&mut self, input: DesktopInput) -> Result<()> {
        self.trace.borrow_mut().observed.push(match input {
            DesktopInput::Key(key) => Observed::Key(key.keysym),
            DesktopInput::Pointer(pointer) => Observed::Pointer {
                x: pointer.x,
                y: pointer.y,
                left_down: pointer.buttons.contains(PointerButton::Left),
            },
        });
        ensure!(self.connected, "cannot send input after disconnect");
        Ok(())
    }

    async fn close(&mut self) {
        self.trace.borrow_mut().observed.push(Observed::Close);
    }
}

// Only the stream owning the next scripted step is ready. This drives the real
// select loop deterministically without sleeps, timeouts, or a real terminal.
fn inputs(trace: Trace) -> impl Stream<Item = io::Result<Event>> {
    stream::poll_fn(move |_| {
        let mut script = trace.borrow_mut();
        match script.steps.front() {
            Some(Step::Input(_) | Step::Resize { .. }) => {
                let event = match script.steps.pop_front().unwrap() {
                    Step::Input(event) => event,
                    Step::Resize { cols, rows } => {
                        script.cols = cols;
                        script.rows = rows;
                        Event::Resize(cols, rows)
                    }
                    _ => unreachable!(),
                };
                Poll::Ready(Some(Ok(event)))
            }
            None => Poll::Ready(None),
            _ => Poll::Pending,
        }
    })
}

fn ticks(trace: Trace) -> impl Stream<Item = ()> {
    stream::poll_fn(move |_| {
        let mut script = trace.borrow_mut();
        match script.steps.front() {
            Some(Step::Refresh | Step::FailDraw) => {
                script.fail_write = matches!(script.steps.pop_front(), Some(Step::FailDraw));
                Poll::Ready(Some(()))
            }
            _ => Poll::Pending,
        }
    })
}

async fn play(steps: impl IntoIterator<Item = Step>) -> (Result<()>, Trace) {
    play_with_options(steps, RenderOptions::default()).await
}

async fn play_with_options(
    steps: impl IntoIterator<Item = Step>,
    initial: RenderOptions,
) -> (Result<()>, Trace) {
    let trace = Rc::new(RefCell::new(Script {
        steps: steps.into_iter().collect(),
        observed: Vec::new(),
        output: Vec::new(),
        cols: 40,
        rows: 20,
        fail_write: false,
    }));
    let desktop = FakeDesktop {
        trace: trace.clone(),
        frame: Frame::new(160, 100),
        fresh_frame: true,
        connected: true,
    };
    let result = run_with(
        desktop,
        MemoryTerminal(trace.clone()),
        ViewerOptions {
            render: RenderMode::Text,
            initial,
        },
        || (inputs(trace.clone()), ticks(trace.clone())),
    )
    .await;
    assert!(
        trace.borrow().steps.is_empty(),
        "session exited before the script finished"
    );
    (result, trace)
}

#[tokio::test]
async fn startup_options_set_the_toolbar_and_frame_placement() {
    for (initial, scale_label, smooth_label, image_start) in [
        (
            RenderOptions::default(),
            "[ Fit=off ]",
            "[ Smooth: on  ]",
            "\x1b[5;2H\x1b[38;2;",
        ),
        (
            RenderOptions {
                scale: ScaleMode::Stretch,
                sampling: Sampling::Nearest,
            },
            "[ Fit=on  ]",
            "[ Smooth: off ]",
            "\x1b[2;2H\x1b[38;2;",
        ),
    ] {
        let (result, trace) = play_with_options([Step::Refresh, quit()], initial).await;
        result.unwrap();
        let script = trace.borrow();
        let output = String::from_utf8_lossy(&script.output);
        assert!(output.contains(scale_label));
        assert!(output.contains(smooth_label));
        assert!(output.contains(image_start));
        assert_restored(&script);
    }
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Step {
    Step::Input(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

fn click(column: u16, row: u16) -> [Step; 2] {
    [
        mouse(MouseEventKind::Down(MouseButton::Left), column, row),
        mouse(MouseEventKind::Up(MouseButton::Left), column, row),
    ]
}

fn quit() -> Step {
    Step::Input(Event::Key(KeyEvent::new(
        KeyCode::F(12),
        KeyModifiers::NONE,
    )))
}

fn assert_restored(script: &Script) {
    let output = String::from_utf8_lossy(&script.output);
    assert!(output.contains("\x1b[?1049h"));
    assert!(output.ends_with("\x1b[?25h\x1b[?1049l"));
    assert!(output.contains("\x1b[?1006l\x1b[?1003l"));
    assert_eq!(script.observed.first(), Some(&Observed::RawMode(true)));
    assert!(
        script
            .observed
            .ends_with(&[Observed::RawMode(false), Observed::Close])
    );
}

#[tokio::test]
async fn toolbar_controls_redraw_locally_and_close_without_forwarding_clicks() {
    let (result, trace) = play(
        [Step::Refresh]
            .into_iter()
            .chain(click(27, 0)) // Fit
            .chain([Step::Refresh])
            .chain(click(11, 0)) // Smooth
            .chain([Step::Refresh])
            .chain(click(36, 0)), // Close
    )
    .await;
    result.unwrap();
    let script = trace.borrow();
    let output = String::from_utf8_lossy(&script.output);
    assert!(output.contains("[ Fit=off ]"));
    assert!(output.contains("[ Fit=on  ]"));
    assert!(output.contains("[ Smooth: off ]"));
    assert!(output.contains("[X]"));
    assert_eq!(
        script.observed,
        [
            Observed::RawMode(true),
            Observed::Refresh,
            Observed::Refresh,
            Observed::Refresh,
            Observed::RawMode(false),
            Observed::Close,
        ]
    );
    assert_restored(&script);
}

#[tokio::test]
async fn pane_resize_redraws_a_static_frame_and_updates_mouse_mapping() {
    let (result, trace) = play(
        [Step::Refresh]
            .into_iter()
            .chain(click(10, 8))
            .chain([Step::Resize { cols: 50, rows: 22 }, Step::Refresh])
            .chain(click(10, 8))
            .chain([quit()]),
    )
    .await;
    result.unwrap();
    let script = trace.borrow();
    let output = String::from_utf8_lossy(&script.output);
    assert_eq!(output.matches("\x1b[2J").count(), 2);
    assert!(output.contains("╭ Test Driver "));
    assert!(!output.contains("╭ VNC "));
    assert!(output.contains("\x1b[5;2H\x1b[38;2;"));
    assert!(output.contains("\x1b[4;2H\x1b[38;2;"));
    assert_eq!(
        script.observed,
        [
            Observed::RawMode(true),
            Observed::Refresh,
            Observed::Pointer {
                x: 40,
                y: 37,
                left_down: true
            },
            Observed::Pointer {
                x: 40,
                y: 37,
                left_down: false
            },
            Observed::Refresh,
            Observed::Pointer {
                x: 31,
                y: 36,
                left_down: true
            },
            Observed::Pointer {
                x: 31,
                y: 36,
                left_down: false
            },
            Observed::RawMode(false),
            Observed::Close,
        ]
    );
    assert_restored(&script);
}

#[tokio::test]
async fn quitting_mid_drag_releases_the_button_before_closing() {
    let (result, trace) = play([
        Step::Refresh,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 8),
        mouse(MouseEventKind::Drag(MouseButton::Left), 11, 8),
        quit(),
    ])
    .await;
    result.unwrap();
    let script = trace.borrow();
    assert_eq!(
        script.observed,
        [
            Observed::RawMode(true),
            Observed::Refresh,
            Observed::Pointer {
                x: 40,
                y: 37,
                left_down: true
            },
            Observed::Pointer {
                x: 44,
                y: 37,
                left_down: true
            },
            Observed::Pointer {
                x: 44,
                y: 37,
                left_down: false
            },
            Observed::RawMode(false),
            Observed::Close,
        ]
    );
    assert_restored(&script);
}

#[tokio::test]
async fn disconnect_restores_the_terminal_even_if_the_button_release_fails() {
    let (result, trace) = play([
        Step::Refresh,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 8),
        Step::Disconnect,
    ])
    .await;
    assert_eq!(result.unwrap_err().to_string(), "scripted disconnect");
    let script = trace.borrow();
    assert!(script.observed.ends_with(&[
        Observed::Pointer {
            x: 40,
            y: 37,
            left_down: false
        },
        Observed::RawMode(false),
        Observed::Close,
    ]));
    assert_restored(&script);
}

#[tokio::test]
async fn draw_failure_restores_the_terminal_and_preserves_the_error() {
    let (result, trace) = play([Step::FailDraw]).await;
    assert_eq!(result.unwrap_err().to_string(), "scripted draw failure");
    assert_restored(&trace.borrow());
}
