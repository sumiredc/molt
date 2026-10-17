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

use crate::ui::{Picked, circled, printable};

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

/// The line of the choice `label` numbered `number`, `❯` before it when in focus. A label may
/// hold a crate name the model wrote, so it is written as text, not as escapes.
pub(super) fn line(number: usize, label: &str, focused: bool) -> String {
    let mark = if focused { '❯' } else { ' ' };
    format!("{mark} {} {}", circled(number), printable(label))
}

/// What the user did at the choices, as keys: picked, or started a line, which the caller reads
/// once raw mode is left.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Keys {
    Picked(Picked),
    /// The line started: its first key and the keys already typed after it, as a paste brings.
    Line(String),
    /// A line typed whole before it could be handed over: a paste that ends with Enter.
    Typed(String),
}

/// Where the keys come from: the terminal, or a list of them in tests.
pub(super) trait Events {
    /// Whether a key has come already, without waiting for one.
    fn pending(&mut self) -> io::Result<bool>;
    /// The next event, waiting for it.
    fn next(&mut self) -> io::Result<Event>;
}

/// The terminal's events, through crossterm.
struct Terminal;

impl Events for Terminal {
    fn pending(&mut self) -> io::Result<bool> {
        event::poll(Duration::ZERO)
    }

    fn next(&mut self) -> io::Result<Event> {
        event::read()
    }
}

/// Lists `choices` under `prompt` and reads keys until one chooses, starts a line, or
/// interrupts. `styled` writes the choice in focus bold.
pub(super) fn read(prompt: &str, choices: &[(usize, &str)], styled: bool) -> io::Result<Keys> {
    let mut out = io::stdout();
    let width = terminal::size().map_or(80, |(columns, _)| usize::from(columns).max(1));
    let _raw = Raw::enter()?;
    let mut events = Terminal;
    drop_pending(&mut events)?;
    let rows = list(&mut out, prompt, choices, styled, width)?;
    keys(&mut events, choices, |focus| {
        draw(&mut out, choices, focus, styled, width, rows).map(|_| ())
    })
}

/// Drops the keys typed while molt worked: they are not an answer to the choices (PRD §8.1).
pub(super) fn drop_pending(events: &mut impl Events) -> io::Result<()> {
    while events.pending()? {
        events.next()?;
    }
    Ok(())
}

/// Reads keys at `choices` until one chooses, starts a line, or interrupts, calling `redraw` with
/// the choice in focus when it moves or is chosen. A number that comes with more keys behind it,
/// as a paste or an input method brings, starts a line instead of choosing (PRD §8.6).
pub(super) fn keys(
    events: &mut impl Events,
    choices: &[(usize, &str)],
    mut redraw: impl FnMut(usize) -> io::Result<()>,
) -> io::Result<Keys> {
    let offered: Vec<usize> = choices.iter().map(|(number, _)| *number).collect();
    let mut focus = 0;
    loop {
        // A change of the terminal's width is not followed: the rows to draw over are counted
        // once (see [`draw`]).
        let Event::Key(key) = events.next()? else {
            continue;
        };
        match action(&key, &offered, focus) {
            Action::Focus(at) if at != focus => {
                focus = at;
                redraw(focus)?;
            }
            Action::Focus(_) | Action::None => {}
            Action::Choose(number) => {
                if let KeyCode::Char(first) = key.code {
                    // Keys pressed behind the number make it a line; other events do not.
                    let behind = pending(events)?;
                    if behind.iter().any(is_press) {
                        return Ok(typed_after(
                            first,
                            behind.into_iter().chain(pending(events)?),
                        ));
                    }
                }
                let at = offered.iter().position(|n| *n == number).unwrap_or(focus);
                redraw(at)?;
                return Ok(Keys::Picked(Picked::Choice(number)));
            }
            Action::Line(first) => return Ok(typed_after(first, pending(events)?)),
            Action::Interrupt => return Ok(Keys::Picked(Picked::Interrupted)),
            Action::Eof => return Ok(Keys::Picked(Picked::Eof)),
        }
    }
}

/// The events that have come already, read without waiting. The keys among them are read here
/// rather than by rustyline, since crossterm may hold them where rustyline cannot read them.
fn pending(events: &mut impl Events) -> io::Result<Vec<Event>> {
    let mut read = Vec::new();
    while events.pending()? {
        read.push(events.next()?);
    }
    Ok(read)
}

fn is_press(event: &Event) -> bool {
    matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press)
}

/// The line started with `first`, and `typed` after it. Lines pasted together become one, their
/// breaks a space; when the last of them ended with Enter, the line is typed whole.
fn typed_after(first: char, typed: impl IntoIterator<Item = Event>) -> Keys {
    let mut line = first.to_string();
    let mut entered = false;
    for event in typed {
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Enter => entered = true,
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if entered && !line.ends_with(' ') {
                    line.push(' ');
                }
                entered = false;
                line.push(c);
            }
            KeyCode::Backspace => {
                line.pop();
            }
            _ => {}
        }
    }
    if entered {
        Keys::Typed(line)
    } else {
        Keys::Line(line)
    }
}

/// Writes `prompt` and draws the choices under it with the first in focus. Returns the rows the
/// choices take, as [`draw`] does.
fn list(
    out: &mut impl Write,
    prompt: &str,
    choices: &[(usize, &str)],
    styled: bool,
    width: usize,
) -> io::Result<u16> {
    write!(out, "{}\r\n", printable(prompt))?;
    draw(out, choices, 0, styled, width, 0)
}

/// Draws the choices and the guide from where the cursor is, after moving up `above` rows to
/// where they were drawn before. Returns the rows they take, the lines wider than the terminal
/// wrapped. Each character is taken as 1 column: molt's words are English, and the item names
/// ASCII. `①` to `④` are of ambiguous width, 2 columns on a terminal set to draw them wide, where
/// a line that just fits may be counted a row short.
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

    #[test]
    fn control_characters_in_the_prompt_and_the_labels_are_not_drawn() {
        let mut out = Vec::new();
        let choices = [(1, "add \u{1b}[31mplanted"), (2, "skip")];
        list(&mut out, "Which \u{1b}[31mplanted?", &choices, true, 80).unwrap();
        draw(&mut out, &choices, 1, true, 80, 2).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("Which [31mplanted?"), "{out:?}");
        assert!(out.contains("add [31mplanted"), "{out:?}");
        assert!(!out.contains("\u{1b}[31mplanted"), "{out:?}");
    }

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

    /// Keys that have all come already.
    struct Typed(std::collections::VecDeque<Event>);

    impl Typed {
        fn of(text: &str) -> Self {
            Self(
                text.chars()
                    .map(|c| match c {
                        '\n' => key(KeyCode::Enter),
                        c => key(KeyCode::Char(c)),
                    })
                    .map(Event::Key)
                    .collect(),
            )
        }
    }

    impl Events for Typed {
        fn pending(&mut self) -> io::Result<bool> {
            Ok(!self.0.is_empty())
        }

        fn next(&mut self) -> io::Result<Event> {
            Ok(self.0.pop_front().expect("a key left"))
        }
    }

    const CHOICES: [(usize, &str); 3] = [(1, "a"), (2, "b"), (4, "d")];

    fn keys_of(text: &str) -> Keys {
        keys(&mut Typed::of(text), &CHOICES, |_| Ok(())).unwrap()
    }

    #[test]
    fn keys_typed_before_the_choices_are_dropped() {
        let mut typed = Typed::of("2\n");
        drop_pending(&mut typed).unwrap();
        assert!(typed.0.is_empty());
    }

    #[test]
    fn a_number_alone_chooses_at_once() {
        assert_eq!(keys_of("2"), Keys::Picked(Picked::Choice(2)));
    }

    #[test]
    fn a_number_with_more_keys_behind_it_starts_a_line_with_them() {
        assert_eq!(keys_of("2行目は？"), Keys::Line("2行目は？".to_owned()));
    }

    #[test]
    fn a_line_pasted_with_enter_is_typed_whole_and_lines_pasted_together_become_one() {
        assert_eq!(keys_of("why?\n"), Keys::Typed("why?".to_owned()));
        assert_eq!(
            keys_of("first\nsecond\n"),
            Keys::Typed("first second".to_owned())
        );
        assert_eq!(keys_of("first\nsec"), Keys::Line("first sec".to_owned()));
        assert_eq!(keys_of("a\n\nb\n"), Keys::Typed("a b".to_owned()));
    }

    #[test]
    fn a_number_with_only_a_release_behind_it_still_chooses() {
        let mut released = key(KeyCode::Char('2'));
        released.kind = KeyEventKind::Release;
        let mut typed = Typed::of("2");
        typed.0.push_back(Event::Key(released));
        typed.0.push_back(Event::Resize(80, 24));

        let picked = keys(&mut typed, &CHOICES, |_| Ok(())).unwrap();

        assert_eq!(picked, Keys::Picked(Picked::Choice(2)));
    }

    #[test]
    fn numbers_are_1_to_4_as_typed_or_full_width() {
        assert_eq!(number('1'), Some(1));
        assert_eq!(number('４'), Some(4));
        assert_eq!(number('5'), None);
        assert_eq!(number('０'), None);
    }
}
