use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use vnc::{VncClient, VncError, X11Event};

use crate::terminal::Viewport;

#[derive(Default)]
pub(crate) struct Pointer {
    buttons: u8,
    position: (u16, u16),
}

impl Pointer {
    pub(crate) async fn send(
        &mut self,
        client: &VncClient,
        mouse: MouseEvent,
        view: Viewport,
    ) -> Result<(), VncError> {
        use MouseEventKind::*;
        let Some(position) = view.point(mouse.column, mouse.row, self.buttons != 0) else {
            return Ok(());
        };
        self.position = position;
        let wheel = match mouse.kind {
            Down(button) | Drag(button) => {
                self.buttons |= button_bit(button);
                0
            }
            Up(button) => {
                self.buttons &= !button_bit(button);
                0
            }
            ScrollUp => 8,
            ScrollDown => 16,
            ScrollLeft => 32,
            ScrollRight => 64,
            Moved => 0,
        };
        self.report(client, self.buttons | wheel).await?;
        if wheel != 0 {
            self.report(client, self.buttons).await?;
        }
        Ok(())
    }

    pub(crate) async fn release(&mut self, client: &VncClient) -> Result<(), VncError> {
        if self.buttons != 0 {
            self.buttons = 0;
            self.report(client, 0).await?;
        }
        Ok(())
    }

    async fn report(&self, client: &VncClient, buttons: u8) -> Result<(), VncError> {
        let (x, y) = self.position;
        client
            .input(X11Event::PointerEvent((x, y, buttons).into()))
            .await
    }
}

fn button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 4,
    }
}

pub(crate) async fn send_key(client: &VncClient, key: KeyEvent) -> Result<(), VncError> {
    let Some(symbol) = keysym(key.code) else {
        return Ok(());
    };
    let mut modifiers = key.modifiers;
    if key.code == KeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }
    // Most terminal key reports give a press but no release. Send a tap, with
    // the indicated modifiers held for its duration.
    let held = [
        (KeyModifiers::SHIFT, 0xffe1),
        (KeyModifiers::CONTROL, 0xffe3),
        (KeyModifiers::ALT, 0xffe9),
        (KeyModifiers::SUPER, 0xffeb),
    ];
    for (flag, key) in held {
        if modifiers.contains(flag) {
            report_key(client, key, true).await?;
        }
    }
    report_key(client, symbol, true).await?;
    report_key(client, symbol, false).await?;
    for (flag, key) in held.into_iter().rev() {
        if modifiers.contains(flag) {
            report_key(client, key, false).await?;
        }
    }
    Ok(())
}

async fn report_key(client: &VncClient, symbol: u32, pressed: bool) -> Result<(), VncError> {
    client
        .input(X11Event::KeyEvent((symbol, pressed).into()))
        .await
}

fn keysym(code: KeyCode) -> Option<u32> {
    use KeyCode::*;
    Some(match code {
        Char(c) if u32::from(c) <= 0xff => c.into(),
        Char(c) => 0x0100_0000 | u32::from(c),
        Backspace => 0xff08,
        Tab | BackTab => 0xff09,
        Enter => 0xff0d,
        Esc => 0xff1b,
        Home => 0xff50,
        Left => 0xff51,
        Up => 0xff52,
        Right => 0xff53,
        Down => 0xff54,
        PageUp => 0xff55,
        PageDown => 0xff56,
        End => 0xff57,
        Insert => 0xff63,
        Delete => 0xffff,
        F(n @ 1..=35) => 0xffbd + u32::from(n),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_terminal_keys_to_x11_keysyms() {
        assert_eq!(keysym(KeyCode::Char('a')), Some(0x61));
        assert_eq!(keysym(KeyCode::Char('€')), Some(0x0100_20ac));
        assert_eq!(keysym(KeyCode::Left), Some(0xff51));
        assert_eq!(keysym(KeyCode::F(1)), Some(0xffbe));
    }
}
