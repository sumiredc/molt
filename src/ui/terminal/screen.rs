//! What a terminal shows while molt works (PRD §8.1): a status line with a spinner, the seconds
//! so far and what molt is doing, kept as the last line under everything printed, and the
//! model's Markdown rendered a line at a time. When stdout is not a terminal, nothing but the
//! text is written: no spinner, style, cursor movement or `\r`. `NO_COLOR` turns off colors only.

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
    /// Whether stdout is a terminal: styles, the spinner and moving the cursor.
    styled: bool,
    /// Whether colors are written (a terminal, and no `NO_COLOR`).
    colors: bool,
    renderer: Renderer,
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
        let styled = io::stdout().is_terminal();
        let colors = styled && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
        Self {
            state: Arc::new(Mutex::new(State::default())),
            styled,
            colors,
            renderer: Renderer::default(),
        }
    }

    pub(super) fn styled(&self) -> bool {
        self.styled
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Prints `text` and a line break above the status line.
    pub(super) fn print(&self, text: &str) {
        let mut state = self.lock();
        let mut out = io::stdout().lock();
        clear(&mut state, &mut out, self.styled);
        let _ = writeln!(out, "{text}");
        draw(&mut state, &mut out, self.styled);
        let _ = out.flush();
    }

    /// Prints `lines` in their tones.
    pub(super) fn print_lines(&self, lines: &[(Tone, String)]) {
        let text: Vec<String> = lines
            .iter()
            .map(|(tone, line)| self.toned(*tone, line))
            .collect();
        self.print(&text.join("\n"));
    }

    fn toned(&self, tone: Tone, line: &str) -> String {
        if !self.styled {
            return line.to_owned();
        }
        match tone {
            Tone::Normal => line.to_owned(),
            Tone::Dim => line.dim().to_string(),
            Tone::Bold => line.bold().to_string(),
        }
    }

    /// Prints a line of the model's Markdown after `indent`, rendered.
    pub(super) fn markdown_line(&mut self, line: &str, indent: &str) {
        let rendered = match self.renderer.line(line) {
            // The fences are not shown: the code's lines are dim instead.
            Styled::Code(None) => return,
            Styled::Code(Some(code)) => self.toned(Tone::Dim, &format!("  {code}")),
            Styled::Heading(text) => self.toned(Tone::Bold, &text),
            Styled::Prose(spans) => spans.iter().map(|span| self.span(span)).collect(),
        };
        self.print(&format!("{indent}{rendered}"));
    }

    fn span(&self, span: &Span) -> String {
        match span {
            Span::Plain(text) => text.clone(),
            Span::Bold(text) => text.as_str().bold().to_string(),
            Span::Italic(text) => text.as_str().italic().to_string(),
            Span::Code(text) if self.colors => text.as_str().cyan().to_string(),
            Span::Code(text) => format!("`{text}`"),
        }
    }

    /// The end of the model's text: the next one starts outside any code block.
    pub(super) fn end_markdown(&mut self) {
        self.renderer = Renderer::default();
    }

    /// Shows what molt is doing, or takes the status line away (`None`). On a terminal only.
    pub(super) fn status(&self, doing: Option<&str>) {
        if !self.styled {
            return;
        }
        let mut state = self.lock();
        let mut out = io::stdout().lock();
        clear(&mut state, &mut out, self.styled);
        state.doing = doing.map(|doing| {
            let since = match &state.doing {
                // The seconds go on when only the words change.
                Some((_, since)) => *since,
                None => Instant::now(),
            };
            (doing.to_owned(), since)
        });
        draw(&mut state, &mut out, self.styled);
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
    let line = format!("{frame} {doing} {}s  (Ctrl-C to stop)", elapsed.as_secs());
    let width = terminal::size().map_or(80, |(columns, _)| usize::from(columns));
    let line: String = line.chars().take(width.saturating_sub(1)).collect();
    let _ = write!(out, "{}", line.dim());
    state.drawn = true;
}
