use crate::terminal::Viewport;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

pub(crate) enum DesktopInput {
    Key(KeyTap),
    Pointer(PointerUpdate),
}

#[derive(Clone, Copy)]
pub(crate) struct KeyTap {
    pub keysym: u32,
    pub modifiers: KeyModifiers,
}

#[derive(Clone, Copy)]
pub(crate) struct PointerUpdate {
    pub x: u16,
    pub y: u16,
    pub buttons: ButtonSet,
    pub scroll: Option<ScrollDirection>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PointerButton {
    Left,
    Middle,
    Right,
}

impl PointerButton {
    pub(crate) const ALL: [Self; 3] = [Self::Left, Self::Middle, Self::Right];

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

impl From<MouseButton> for PointerButton {
    fn from(button: MouseButton) -> Self {
        match button {
            MouseButton::Left => Self::Left,
            MouseButton::Middle => Self::Middle,
            MouseButton::Right => Self::Right,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct ButtonSet(u8);

impl ButtonSet {
    pub(crate) fn contains(self, button: PointerButton) -> bool {
        self.0 & button.bit() != 0
    }

    pub(crate) fn set(&mut self, button: PointerButton, pressed: bool) {
        if pressed {
            self.0 |= button.bit();
        } else {
            self.0 &= !button.bit();
        }
    }

    fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Default)]
pub(crate) struct Pointer {
    buttons: ButtonSet,
    position: (u16, u16),
}

impl Pointer {
    pub(crate) fn update(&mut self, mouse: MouseEvent, view: Viewport) -> Option<PointerUpdate> {
        use MouseEventKind::*;
        let captured = match mouse.kind {
            Drag(button) | Up(button) if !self.buttons.contains(button.into()) => return None,
            Drag(_) | Up(_) => true,
            _ => false,
        };
        self.position = view.point(mouse.column, mouse.row, captured)?;
        let scroll = match mouse.kind {
            Down(button) => {
                self.buttons.set(button.into(), true);
                None
            }
            Up(button) => {
                self.buttons.set(button.into(), false);
                None
            }
            ScrollUp => Some(ScrollDirection::Up),
            ScrollDown => Some(ScrollDirection::Down),
            ScrollLeft => Some(ScrollDirection::Left),
            ScrollRight => Some(ScrollDirection::Right),
            Moved | Drag(_) => None,
        };
        Some(self.report(scroll))
    }

    pub(crate) fn release(&mut self) -> Option<PointerUpdate> {
        if !self.buttons.is_empty() {
            self.buttons = ButtonSet::default();
            Some(self.report(None))
        } else {
            None
        }
    }

    fn report(&self, scroll: Option<ScrollDirection>) -> PointerUpdate {
        let (x, y) = self.position;
        PointerUpdate {
            x,
            y,
            buttons: self.buttons,
            scroll,
        }
    }
}

pub(crate) const KEY_MODIFIERS: [(KeyModifiers, u32); 4] = [
    (KeyModifiers::SHIFT, 0xffe1),
    (KeyModifiers::CONTROL, 0xffe3),
    (KeyModifiers::ALT, 0xffe9),
    (KeyModifiers::SUPER, 0xffeb),
];

pub(crate) fn key_tap(key: KeyEvent) -> Option<KeyTap> {
    let keysym = keysym(key.code)?;
    let mut modifiers = key.modifiers;
    if key.code == KeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }
    Some(KeyTap { keysym, modifiers })
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
