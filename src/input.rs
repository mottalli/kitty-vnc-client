use crate::PointerUpdate;
use crate::terminal::Viewport;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

#[derive(Default)]
pub(crate) struct Pointer {
    buttons: u8,
    position: (u16, u16),
}

impl Pointer {
    pub(crate) fn update(&mut self, mouse: MouseEvent, view: Viewport) -> Option<PointerUpdate> {
        use MouseEventKind::*;
        let captured = match mouse.kind {
            Drag(button) | Up(button) if self.buttons & button_bit(button) == 0 => return None,
            Drag(_) | Up(_) => true,
            _ => false,
        };
        self.position = view.point(mouse.column, mouse.row, captured)?;
        let wheel = match mouse.kind {
            Down(button) => {
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
            Moved | Drag(_) => 0,
        };
        Some(self.report(wheel))
    }

    pub(crate) fn release(&mut self) -> Option<PointerUpdate> {
        if self.buttons != 0 {
            self.buttons = 0;
            Some(self.report(0))
        } else {
            None
        }
    }

    fn report(&self, wheel: u8) -> PointerUpdate {
        let (x, y) = self.position;
        PointerUpdate {
            x,
            y,
            buttons: self.buttons,
            wheel,
        }
    }
}

fn button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 4,
    }
}

pub const KEY_MODIFIERS: [(KeyModifiers, u32); 4] = [
    (KeyModifiers::SHIFT, 0xffe1),
    (KeyModifiers::CONTROL, 0xffe3),
    (KeyModifiers::ALT, 0xffe9),
    (KeyModifiers::SUPER, 0xffeb),
];

pub fn key_tap(key: KeyEvent) -> Option<(u32, KeyModifiers)> {
    let symbol = keysym(key.code)?;
    let mut modifiers = key.modifiers;
    if key.code == KeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }
    Some((symbol, modifiers))
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
