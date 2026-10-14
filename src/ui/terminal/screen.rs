//! What a terminal shows while molt works (PRD §8.1): a status line with a spinner, the seconds
//! so far and what molt is doing, kept as the last line under everything printed, and the
//! model's Markdown rendered a line at a time. When stdout is not a terminal, nothing but the
//! text is written: no spinner, style, cursor movement or `\r`. `NO_COLOR` turns off colors only.

use std::ffi::OsStr;
use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::style::Stylize;
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{cursor, queue};

use super::super::Tone;
use super::super::markdown::{Renderer, Span, Styled};

/// The spinner's frames.
const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// How often the spinner turns.
const TICK: Duration = Duration::from_millis(100);

/// The output of a terminal, shared with the thread that turns the spinner.
pub(super) struct Screen {
    state: Arc<Mutex<State>>,
    style: Style,
    renderer: Renderer,
}

/// How text is written to stdout.
#[derive(Debug, Clone, Copy)]
struct Style {
    /// Whether stdout is a terminal: styles, the spinner and moving the cursor.
    styled: bool,
    /// Whether colors are written: a terminal, and `NO_COLOR` unset or empty.
    colors: bool,
}

impl Style {
    fn new(terminal: bool, no_color: Option<&OsStr>) -> Self {
        Self {
            styled: terminal,
            colors: terminal && no_color.is_none_or(OsStr::is_empty),
        }
    }

    fn toned(self, tone: Tone, line: &str) -> String {
        if !self.styled {
            return line.to_owned();
        }
        match tone {
            Tone::Normal => line.to_owned(),
            Tone::Dim => line.dim().to_string(),
            Tone::Bold => line.bold().to_string(),
        }
    }

    fn span(self, span: &Span) -> String {
        match span {
            Span::Plain(text) => text.clone(),
            Span::Bold(text) if self.styled => text.as_str().bold().to_string(),
            Span::Italic(text) if self.styled => text.as_str().italic().to_string(),
            Span::Code(text) if self.colors => text.as_str().cyan().to_string(),
            Span::Bold(text) => format!("**{text}**"),
            Span::Italic(text) => format!("*{text}*"),
            Span::Code(text) => format!("`{text}`"),
        }
    }

    /// `line` of the model's Markdown, rendered by `renderer`, or `None` for a fence, which is not
    /// shown: the code's lines are dim instead.
    fn markdown(self, renderer: &mut Renderer, line: &str) -> Option<String> {
        Some(match renderer.line(line) {
            Styled::Code(None) => return None,
            Styled::Code(Some(code)) => self.toned(Tone::Dim, &format!("  {code}")),
            Styled::Heading(text) => self.toned(Tone::Bold, &text),
            Styled::Prose(spans) => spans.iter().map(|span| self.span(span)).collect(),
        })
    }
}

#[derive(Default)]
struct State {
    /// What molt is doing, and since when.
    doing: Option<(String, Instant)>,
    /// Whether the status line is on the screen now.
    drawn: bool,
    /// Whether the spinner's thread runs.
    turning: bool,
}

impl Screen {
    pub(super) fn new() -> Self {
        let no_color = std::env::var_os("NO_COLOR");
        Self {
            state: Arc::new(Mutex::new(State::default())),
            style: Style::new(io::stdout().is_terminal(), no_color.as_deref()),
            renderer: Renderer::default(),
        }
    }

    pub(super) fn styled(&self) -> bool {
        self.style.styled
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Prints `text` and a line break above the status line.
    pub(super) fn print(&self, text: &str) {
        let mut state = self.lock();
        let mut out = io::stdout().lock();
        clear(&mut state, &mut out, self.style.styled);
        let _ = writeln!(out, "{text}");
        draw(&mut state, &mut out, self.style.styled);
        let _ = out.flush();
    }

    /// Prints `lines` in their tones.
    pub(super) fn print_lines(&self, lines: &[(Tone, String)]) {
        let text: Vec<String> = lines
            .iter()
            .map(|(tone, line)| self.style.toned(*tone, line))
            .collect();
        self.print(&text.join("\n"));
    }

    /// Prints a line of the model's Markdown after `indent`, rendered.
    pub(super) fn markdown_line(&mut self, line: &str, indent: &str) {
        let line = super::super::printable(line);
        if let Some(rendered) = self.style.markdown(&mut self.renderer, &line) {
            if rendered.is_empty() {
                self.print("");
            } else {
                self.print(&format!("{indent}{rendered}"));
            }
        }
    }

    /// The end of the model's text: the next one starts outside any code block.
    pub(super) fn end_markdown(&mut self) {
        self.renderer = Renderer::default();
    }

    /// Shows what molt is doing, or takes the status line away (`None`). On a terminal only.
    pub(super) fn status(&self, doing: Option<&str>) {
        if !self.style.styled {
            return;
        }
        let mut state = self.lock();
        let mut out = io::stdout().lock();
        clear(&mut state, &mut out, self.style.styled);
        state.doing = doing.map(|doing| {
            let since = match &state.doing {
                // The seconds go on when only the words change.
                Some((_, since)) => *since,
                None => Instant::now(),
            };
            (doing.to_owned(), since)
        });
        draw(&mut state, &mut out, self.style.styled);
        let _ = out.flush();
        if state.doing.is_some() && !state.turning {
            state.turning = true;
            let shared = Arc::clone(&self.state);
            thread::spawn(move || turn(&shared));
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.status(None);
    }
}

fn lock(state: &Arc<Mutex<State>>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Turns the spinner until there is nothing to show.
fn turn(shared: &Arc<Mutex<State>>) {
    loop {
        thread::sleep(TICK);
        let mut state = lock(shared);
        if state.doing.is_none() {
            state.turning = false;
            return;
        }
        let mut out = io::stdout().lock();
        clear(&mut state, &mut out, true);
        draw(&mut state, &mut out, true);
        let _ = out.flush();
    }
}

/// Takes the status line off the screen, if it is on it.
fn clear(state: &mut State, out: &mut impl Write, styled: bool) {
    if styled && state.drawn {
        let _ = queue!(out, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine));
        state.drawn = false;
    }
}

/// Draws the status line, if molt is doing something: the spinner, the words and the seconds,
/// cut to the terminal's width. Its words are molt's own English and the seconds, so their width
/// is their number of characters (PRD §8.1).
fn draw(state: &mut State, out: &mut impl Write, styled: bool) {
    let Some((doing, since)) = &state.doing else {
        return;
    };
    if !styled {
        return;
    }
    let elapsed = since.elapsed();
    let frame = FRAMES[(elapsed.as_millis() / TICK.as_millis()) as usize % FRAMES.len()];
    let line = format!("{frame} {doing} {}s", elapsed.as_secs());
    let width = terminal::size().map_or(80, |(columns, _)| usize::from(columns));
    let line: String = line.chars().take(width.saturating_sub(1)).collect();
    let _ = write!(out, "{}", line.dim());
    state.drawn = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `text` has an escape that sets a color (SGR 30–49, 90–107, or 38/48 with more).
    fn has_color(text: &str) -> bool {
        text.split("\x1b[").skip(1).any(|sequence| {
            let params = sequence.split('m').next().unwrap_or_default();
            params
                .split(';')
                .any(|param| matches!(param.parse::<u8>(), Ok(30..=49 | 90..=107)))
        })
    }

    fn rendered(style: Style, lines: &[&str]) -> String {
        let mut renderer = Renderer::default();
        let mut text: Vec<String> = lines
            .iter()
            .filter_map(|line| style.markdown(&mut renderer, line))
            .collect();
        text.push(style.toned(Tone::Dim, "a side note"));
        text.join("\n")
    }

    const MARKDOWN: [&str; 6] = [
        "## Heading",
        "Use `dbg!` **here**, *once*.",
        "- item",
        "```rust",
        "fn main() {}",
        "```",
    ];

    #[test]
    fn with_no_color_a_terminal_gets_styles_but_no_color() {
        let text = rendered(Style::new(true, Some(OsStr::new("1"))), &MARKDOWN);

        assert!(text.contains('\x1b'), "dim and bold stay: {text:?}");
        assert!(!has_color(&text), "{text:?}");
    }

    #[test]
    fn an_empty_no_color_does_not_turn_colors_off_and_output_not_to_a_terminal_has_none() {
        // The decision alone: crossterm reads `NO_COLOR` from the environment too, so the
        // rendered colors depend on where the tests run.
        assert!(Style::new(true, Some(OsStr::new(""))).colors);
        assert!(Style::new(true, None).colors);
        assert!(!Style::new(true, Some(OsStr::new("1"))).colors);
        assert!(!Style::new(false, None).colors);
    }

    #[test]
    fn output_that_is_not_a_terminal_has_no_escape_and_keeps_its_markers() {
        let text = rendered(Style::new(false, None), &MARKDOWN);

        assert!(!text.contains('\x1b') && !text.contains('\r'), "{text:?}");
        assert!(text.contains("`dbg!` **here**, *once*"), "{text:?}");
    }
}
