//! The choices at a boundary on a terminal (PRD §8.6): keys read in crossterm's raw mode, where ↑↓
//! and Enter choose, and so does the number of a choice listed, at once. Any other key starts a
//! line with it, edited by rustyline. Keys typed before the choices were listed are dropped, so
//! that an Enter or a number typed while molt worked does not choose.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::queue;
use crossterm::style::Stylize;
use crossterm::terminal::{self, Clear, ClearType, disable_raw_mode, enable_raw_mode};

use crate::ui::{Picked, circled};

/// What molt says under the choices.
const GUIDE: &str =
    "↑↓ and Enter, or a number, to choose. Type anything else to ask or give a command.";

/// What a key does at the choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    /// Moves the focus to the choice at this place in the list.
    Focus(usize),
    /// Chooses the choice of this number.
    Choose(usize),
    /// Starts a line with this character in it.
    Line(char),
    Interrupt,
    Eof,
    /// Nothing: a key that does nothing here.
    None,
}

/// What `key` does at the choices numbered `offered`, the one at `focus` in focus.
pub(super) fn action(key: &KeyEvent, offered: &[usize], focus: usize) -> Action {
    if key.kind != KeyEventKind::Press {
        return Action::None;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if control => Action::Interrupt,
        KeyCode::Char('d') if control => Action::Eof,
        KeyCode::Char(_) if control => Action::None,
        KeyCode::Up => Action::Focus(focus.saturating_sub(1)),
        KeyCode::Down => Action::Focus((focus + 1).min(offered.len().saturating_sub(1))),
        KeyCode::Enter => offered
            .get(focus)
            .map_or(Action::None, |n| Action::Choose(*n)),
        KeyCode::Char(c) => match number(c) {
            Some(n) if offered.contains(&n) => Action::Choose(n),
            _ => Action::Line(c),
        },
        _ => Action::None,
    }
}

/// The number `c` stands for, `1` to `4`, as typed or as an input method types it (`１` to `４`).
pub(super) fn number(c: char) -> Option<usize> {
    match c {
        '1'..='4' => c.to_digit(10).map(|d| d as usize),
        '１'..='４' => Some(c as usize - '１' as usize + 1),
        _ => None,
    }
}

/// The line of the choice `label` numbered `number`, `❯` before it when in focus.
pub(super) fn line(number: usize, label: &str, focused: bool) -> String {
    let mark = if focused { '❯' } else { ' ' };
    format!("{mark} {} {label}", circled(number))
}

/// What the user did at the choices, as keys: picked, or started a line, which the caller reads
/// once raw mode is left.
pub(super) enum Keys {
    Picked(Picked),
    /// The line started: its first key and the keys already typed after it, as a paste brings.
    Line(String),
    /// A line typed whole before it could be handed over: a paste that ends with Enter.
    Typed(String),
}

/// Lists `choices` under `prompt` and reads keys until one chooses, starts a line, or
/// interrupts. `styled` writes the choice in focus bold.
pub(super) fn read(prompt: &str, choices: &[(usize, &str)], styled: bool) -> io::Result<Keys> {
    let mut out = io::stdout();
    let offered: Vec<usize> = choices.iter().map(|(number, _)| *number).collect();
    let width = terminal::size().map_or(80, |(columns, _)| usize::from(columns).max(1));
    let _raw = Raw::enter()?;
    // Keys typed while molt worked are not an answer to the choices.
    while event::poll(Duration::ZERO)? {
        event::read()?;
    }
    write!(out, "{prompt}\r\n")?;
    let mut focus = 0;
    let rows = draw(&mut out, choices, focus, styled, width, 0)?;
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        match action(&key, &offered, focus) {
            Action::Focus(at) if at != focus => {
                focus = at;
                draw(&mut out, choices, focus, styled, width, rows)?;
            }
            Action::Focus(_) | Action::None => {}
            Action::Choose(number) => {
                let at = offered.iter().position(|n| *n == number).unwrap_or(focus);
                draw(&mut out, choices, at, styled, width, rows)?;
                return Ok(Keys::Picked(Picked::Choice(number)));
            }
            Action::Line(first) => return typed_after(first),
            Action::Interrupt => return Ok(Keys::Picked(Picked::Interrupted)),
            Action::Eof => return Ok(Keys::Picked(Picked::Eof)),
        }
    }
}

/// The line started with `first`, and the keys already typed after it: they are read here, since
/// crossterm may hold them where rustyline cannot read them.
fn typed_after(first: char) -> io::Result<Keys> {
    let mut line = first.to_string();
    while event::poll(Duration::ZERO)? {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Enter => return Ok(Keys::Typed(line)),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => line.push(c),
            KeyCode::Backspace => {
                line.pop();
            }
            _ => {}
        }
    }
    Ok(Keys::Line(line))
}

/// Draws the choices and the guide from where the cursor is, after moving up `above` rows to
/// where they were drawn before. Returns the rows they take, the lines wider than the terminal
/// wrapped: molt's words are English, so their width is their number of characters.
fn draw(
    out: &mut impl Write,
    choices: &[(usize, &str)],
    focus: usize,
    styled: bool,
    width: usize,
    above: u16,
) -> io::Result<u16> {
    if above > 0 {
        queue!(out, MoveUp(above), MoveToColumn(0))?;
    }
    queue!(out, Clear(ClearType::FromCursorDown))?;
    let mut rows = 0;
    let mut lines: Vec<String> = Vec::new();
    for (at, (number, label)) in choices.iter().enumerate() {
        let text = line(*number, label, at == focus);
        rows += text.chars().count().div_ceil(width).max(1);
        lines.push(if styled && at == focus {
            text.bold().to_string()
        } else {
            text
        });
    }
    rows += GUIDE.chars().count().div_ceil(width).max(1);
    lines.push(if styled {
        GUIDE.dim().to_string()
    } else {
        GUIDE.to_owned()
    });
    for text in lines {
        // `\r` too: in raw mode a line break does not go back to the first column.
        write!(out, "{text}\r\n")?;
    }
    out.flush()?;
    Ok(u16::try_from(rows).unwrap_or(u16::MAX))
}

/// Raw mode, left when dropped.
struct Raw;

impl Raw {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    const OFFERED: [usize; 3] = [1, 2, 4];

    #[test]
    fn up_and_down_move_the_focus_within_the_list_and_enter_chooses_it() {
        assert_eq!(action(&key(KeyCode::Down), &OFFERED, 0), Action::Focus(1));
        assert_eq!(action(&key(KeyCode::Down), &OFFERED, 2), Action::Focus(2));
        assert_eq!(action(&key(KeyCode::Up), &OFFERED, 0), Action::Focus(0));
        assert_eq!(action(&key(KeyCode::Enter), &OFFERED, 2), Action::Choose(4));
    }

    #[test]
    fn the_number_of_a_choice_listed_chooses_it_at_once_also_full_width() {
        assert_eq!(
            action(&key(KeyCode::Char('2')), &OFFERED, 0),
            Action::Choose(2)
        );
        assert_eq!(
            action(&key(KeyCode::Char('４')), &OFFERED, 0),
            Action::Choose(4)
        );
    }

    #[test]
    fn a_number_not_listed_and_any_other_key_start_a_line_with_it() {
        assert_eq!(
            action(&key(KeyCode::Char('3')), &OFFERED, 0),
            Action::Line('3')
        );
        assert_eq!(
            action(&key(KeyCode::Char('w')), &OFFERED, 0),
            Action::Line('w')
        );
        assert_eq!(
            action(&key(KeyCode::Char('/')), &OFFERED, 0),
            Action::Line('/')
        );
        assert_eq!(
            action(&key(KeyCode::Char('な')), &OFFERED, 0),
            Action::Line('な')
        );
    }

    #[test]
    fn ctrl_c_and_ctrl_d_come_back_as_such() {
        assert_eq!(action(&ctrl('c'), &OFFERED, 0), Action::Interrupt);
        assert_eq!(action(&ctrl('d'), &OFFERED, 0), Action::Eof);
        assert_eq!(action(&ctrl('x'), &OFFERED, 0), Action::None);
    }

    #[test]
    fn a_released_key_does_nothing() {
        let mut released = key(KeyCode::Enter);
        released.kind = KeyEventKind::Release;
        assert_eq!(action(&released, &OFFERED, 0), Action::None);
    }

    #[test]
    fn the_choice_in_focus_is_marked() {
        assert_eq!(
            line(2, "Write it without them", true),
            "❯ ② Write it without them"
        );
        assert_eq!(line(4, "Next", false), "  ④ Next");
    }

    #[test]
    fn numbers_are_1_to_4_as_typed_or_full_width() {
        assert_eq!(number('1'), Some(1));
        assert_eq!(number('４'), Some(4));
        assert_eq!(number('5'), None);
        assert_eq!(number('０'), None);
    }
}
