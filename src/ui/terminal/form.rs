//! The [`Form`] at a terminal: keys read in crossterm's raw mode, on the alternate screen (PRD
//! §8.1). There Ctrl-C comes as a key, not as SIGINT, and interrupts the form. The terminal is
//! restored however the form ends, a panic included.

use std::io::{self, IsTerminal, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode,
};
use crossterm::{execute, queue};

use crate::ui::UiError;
use crate::ui::form::{Form, FormState, FormValues, Key, Outcome, Screen, render};

/// Lets the user fill in `form` at the terminal. `None` when the user interrupted.
pub(super) fn fill_in(
    form: &Form,
    initial: Option<&FormValues>,
) -> Result<Option<FormValues>, UiError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(UiError::NotATerminal);
    }
    run(form, initial).map_err(UiError::Terminal)
}

fn run(form: &Form, initial: Option<&FormValues>) -> io::Result<Option<FormValues>> {
    let _raw = RawScreen::enter()?;
    let mut out = io::stdout();
    let mut state = FormState::new(form, initial);
    loop {
        let (width, _) = terminal::size()?;
        draw(&mut out, &render(&state, usize::from(width)))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        let Some(key) = key_of(key) else {
            continue;
        };
        match state.press(key) {
            Outcome::Continue => {}
            Outcome::Submit => return Ok(Some(state.into_values())),
            Outcome::Interrupt => return Ok(None),
        }
    }
}

/// Raw mode on the alternate screen, left when dropped.
struct RawScreen;

impl RawScreen {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        // From here on, dropping restores the terminal, also when entering fails halfway.
        let screen = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(screen)
    }
}

impl Drop for RawScreen {
    fn drop(&mut self) {
        // Nothing more can be done if restoring fails.
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

fn draw(out: &mut impl Write, screen: &Screen) -> io::Result<()> {
    queue!(out, Hide, MoveTo(0, 0), Clear(ClearType::All))?;
    for (row, line) in screen.lines.iter().enumerate() {
        queue!(out, MoveTo(0, to_u16(row)))?;
        out.write_all(line.as_bytes())?;
    }
    if let Some((row, column)) = screen.cursor {
        queue!(out, MoveTo(to_u16(column), to_u16(row)), Show)?;
    }
    out.flush()
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// The form's key for a key pressed, if it is one the form acts on.
fn key_of(event: KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let control = event.modifiers.contains(KeyModifiers::CONTROL);
    Some(match event.code {
        KeyCode::Char('c' | 'd') if control => Key::Interrupt,
        KeyCode::Char(_) if control || event.modifiers.contains(KeyModifiers::ALT) => return None,
        KeyCode::Char(' ') => Key::Space,
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_c_and_ctrl_d_interrupt() {
        for c in ['c', 'd'] {
            assert_eq!(
                key_of(pressed(KeyCode::Char(c), KeyModifiers::CONTROL)),
                Some(Key::Interrupt)
            );
        }
    }

    #[test]
    fn plain_and_shifted_chars_are_typed_and_other_chords_are_not() {
        assert_eq!(
            key_of(pressed(KeyCode::Char('あ'), KeyModifiers::NONE)),
            Some(Key::Char('あ'))
        );
        assert_eq!(
            key_of(pressed(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Some(Key::Char('A'))
        );
        assert_eq!(
            key_of(pressed(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(key_of(pressed(KeyCode::Char('x'), KeyModifiers::ALT)), None);
    }

    #[test]
    fn space_tab_and_back_tab_are_their_own_keys() {
        assert_eq!(
            key_of(pressed(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(Key::Space)
        );
        assert_eq!(
            key_of(pressed(KeyCode::Tab, KeyModifiers::NONE)),
            Some(Key::Tab)
        );
        assert_eq!(
            key_of(pressed(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Key::BackTab)
        );
    }

    #[test]
    fn a_released_key_does_nothing() {
        let mut event = pressed(KeyCode::Enter, KeyModifiers::NONE);
        event.kind = KeyEventKind::Release;
        assert_eq!(key_of(event), None);
    }
}
