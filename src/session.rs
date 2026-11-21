use std::future::pending;
use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use crossterm::event::{Event, EventStream};
use futures_util::{Stream, StreamExt, pin_mut, stream};
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::input::DesktopInput;
use crate::terminal::{Console, TerminalIo};
use crate::viewer::{Viewer, ViewerEvent};
use crate::{Frame, RenderMode};

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub(crate) enum DesktopUpdate {
    Unchanged,
    FrameChanged,
    Resized,
}

/// Backends own their framebuffers. Every partial update must be applied before
/// reporting a change; the viewer may then render only the latest complete buffer.
pub(crate) trait DesktopBackend {
    /// Short ASCII label for the frame, defined by the backend rather than the server.
    fn frame_title(&self) -> &'static str;

    fn frame(&self) -> Option<&Frame>;

    /// Push-driven sources override this. Polling sources refresh on a timer.
    /// The call must be safe to cancel when another event wins the select.
    async fn next_update(&mut self) -> Result<DesktopUpdate> {
        pending().await
    }

    async fn refresh(&mut self) -> Result<DesktopUpdate>;
    async fn send_input(&mut self, input: DesktopInput) -> Result<()>;

    async fn close(&mut self) {}
}

pub fn require_terminal() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "run this directly in an interactive terminal pane"
    );
    Ok(())
}

pub(crate) async fn run(desktop: impl DesktopBackend, render: RenderMode) -> Result<()> {
    run_with(desktop, Console::new(), render, || {
        let mut refresh = time::interval_at(Instant::now() + REFRESH_INTERVAL, REFRESH_INTERVAL);
        refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let ticks = stream::unfold(refresh, |mut refresh| async move {
            refresh.tick().await;
            Some(((), refresh))
        });
        (EventStream::new(), ticks)
    })
    .await
}

async fn run_with<E, T>(
    mut desktop: impl DesktopBackend,
    io: impl TerminalIo,
    render: RenderMode,
    sources: impl FnOnce() -> (E, T),
) -> Result<()>
where
    E: Stream<Item = io::Result<Event>>,
    T: Stream<Item = ()>,
{
    let result = async {
        let mut viewer = Viewer::open(io, render, desktop.frame_title())?;
        let (events, ticks) = sources();
        let result = drive(&mut desktop, &mut viewer, events, ticks).await;
        if let Some(release) = viewer.reset_pointer() {
            // Disconnects and failed rendering still restore the terminal.
            let _ = time::timeout(
                SHUTDOWN_TIMEOUT,
                desktop.send_input(DesktopInput::Pointer(release)),
            )
            .await;
        }
        result
    }
    .await;
    let _ = time::timeout(SHUTDOWN_TIMEOUT, desktop.close()).await;
    result
}

async fn drive(
    desktop: &mut impl DesktopBackend,
    viewer: &mut Viewer<impl TerminalIo>,
    events: impl Stream<Item = io::Result<Event>>,
    ticks: impl Stream<Item = ()>,
) -> Result<()> {
    pin_mut!(events, ticks);
    let mut redraw = false;

    loop {
        tokio::select! {
            event = events.next() => match viewer.handle(event.context("terminal input closed")??) {
                ViewerEvent::Quit => return Ok(()),
                ViewerEvent::Input(input) => desktop.send_input(input).await?,
                ViewerEvent::None => {},
            },
            update = desktop.next_update() => {
                observe(desktop, viewer, &mut redraw, update?).await?;
            },
            tick = ticks.next() => {
                tick.context("refresh clock closed")?;
                let update = desktop.refresh().await?;
                observe(desktop, viewer, &mut redraw, update).await?;
                if let Some(release) = viewer.render(desktop.frame(), redraw)? {
                    desktop.send_input(DesktopInput::Pointer(release)).await?;
                }
                redraw = false;
            },
        }
    }
}

async fn observe(
    desktop: &mut impl DesktopBackend,
    viewer: &mut Viewer<impl TerminalIo>,
    redraw: &mut bool,
    update: DesktopUpdate,
) -> Result<()> {
    match update {
        DesktopUpdate::Unchanged => {}
        DesktopUpdate::FrameChanged => *redraw = true,
        DesktopUpdate::Resized => {
            if let Some(release) = viewer.reset_pointer() {
                desktop.send_input(DesktopInput::Pointer(release)).await?;
            }
            *redraw = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
