use std::io;

use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};

use crate::input::{DesktopInput, Pointer, PointerUpdate, key_tap};
use crate::terminal::{Control, Pane, RenderOptions, Terminal, TerminalIo, Viewport};
use crate::{Frame, RenderMode};

pub(crate) enum ViewerEvent {
    None,
    Quit,
    Input(DesktopInput),
}

pub(crate) struct Viewer<I: TerminalIo> {
    terminal: Terminal<I>,
    pane: Pane,
    options: RenderOptions,
    viewport: Option<Viewport>,
    pointer: Pointer,
    pressed: Option<Control>,
    dirty: bool,
}

impl<I: TerminalIo> Viewer<I> {
    pub(crate) fn open(io: I, mode: RenderMode) -> io::Result<Self> {
        let mut terminal = Terminal::open(io, mode)?;
        let pane = terminal.pane()?;
        let options = RenderOptions::default();
        let viewport = terminal.draw(pane, None, options)?;
        Ok(Self {
            terminal,
            pane,
            options,
            viewport,
            pointer: Pointer::default(),
            pressed: None,
            dirty: false,
        })
    }

    pub(crate) fn render(
        &mut self,
        frame: Option<&Frame>,
        changed: bool,
    ) -> io::Result<Option<PointerUpdate>> {
        // Pixel dimensions can change without the row or column counts changing.
        let pane = self.terminal.pane()?;
        if changed || self.dirty || self.pane != pane {
            self.viewport = self.terminal.draw(pane, frame, self.options)?;
            self.pane = pane;
            self.dirty = false;
            if self.viewport.is_none() {
                return Ok(self.pointer.release());
            }
        }
        Ok(None)
    }

    pub(crate) fn reset_pointer(&mut self) -> Option<PointerUpdate> {
        self.viewport = None;
        self.pointer.release()
    }

    pub(crate) fn handle(&mut self, event: Event) -> ViewerEvent {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::F(12) {
                    ViewerEvent::Quit
                } else if let Some(tap) = key_tap(key) {
                    ViewerEvent::Input(DesktopInput::Key(tap))
                } else {
                    ViewerEvent::None
                }
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Resize(..) => {
                self.dirty = true;
                ViewerEvent::None
            }
            _ => ViewerEvent::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewerEvent {
        let hit = self.pane.control_at(mouse.column, mouse.row, self.options);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if hit.is_some() => self.pressed = hit,
            MouseEventKind::Drag(MouseButton::Left) if self.pressed.is_some() => {}
            MouseEventKind::Up(MouseButton::Left) if self.pressed.is_some() => {
                if self.pressed.take() == hit {
                    match hit {
                        Some(Control::Close) => return ViewerEvent::Quit,
                        Some(Control::Scale) => {
                            self.options.scale = self.options.scale.toggled();
                            self.dirty = true;
                        }
                        Some(Control::Smoothing) => {
                            self.options.sampling = self.options.sampling.toggled();
                            self.dirty = true;
                        }
                        None => {}
                    }
                }
            }
            _ => {
                if let Some(update) = self
                    .viewport
                    .and_then(|view| self.pointer.update(mouse, view))
                {
                    return ViewerEvent::Input(DesktopInput::Pointer(update));
                }
            }
        }
        ViewerEvent::None
    }
}
