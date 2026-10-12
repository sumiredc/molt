//! A form of tabs the user moves through freely before submitting it all at once (PRD §8.6, the
//! form of `molt init`).
//!
//! The form is generic: it knows tabs, choices, a checklist and a question, not categories or
//! elements, and holds no fixed strings; the caller passes them all, in the language it chose
//! (PRD §8.2, §3). Each tab has a choice; one of its choices shows a checklist, and once an item is
//! checked, a one-line answer to a question about the checked items. What is entered is never
//! dropped while the form is open: a checklist hidden by another choice keeps its checks and
//! answer, and they come back with the choice. What to make of the values is the caller's.
//!
//! [`FormState`] is the state and its moves by key, without a terminal, so that they are tested
//! with key sequences (PRD §9); [`render`] lays it out as lines for the terminal to draw.

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use unicode_width::UnicodeWidthChar;

/// Where a tab's question takes the names of the checked items.
pub const ITEMS: &str = "{items}";

/// A form: its tabs, then a last tab to submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub tabs: Vec<Tab>,
    pub text: FormText,
}

/// One tab of a [`Form`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// The tab's name in the row of tabs.
    pub title: String,
    /// What the choices answer.
    pub prompt: String,
    /// The choices; at least one.
    pub choices: Vec<String>,
    /// The index of the choice that shows the checklist.
    pub checklist_on: usize,
    /// What the checklist asks.
    pub checklist_prompt: String,
    /// The items of the checklist.
    pub items: Vec<String>,
    /// The question about the checked items, with [`ITEMS`] where their names go.
    pub question: String,
    /// Lines of material per item, shown under the question for each checked item. Shorter than
    /// `items` when the last items have none.
    pub material: Vec<Vec<String>>,
}

/// The strings of a [`Form`] that are not a tab's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormText {
    /// The name of the last tab.
    pub submit_title: String,
    /// The keys, shown at the bottom of every tab.
    pub guide: String,
    /// Heads the list of tabs without a choice, on the last tab.
    pub no_choice: String,
    /// Heads the list of tabs with checks but a blank answer, on the last tab.
    pub blank_answer: String,
    /// Says that Enter submits, on the last tab.
    pub ready: String,
    /// Says that every tab needs a choice first, on the last tab.
    pub not_ready: String,
}

/// What is entered in one tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabValues {
    /// The index of the chosen choice.
    pub choice: Option<usize>,
    /// The indexes of the checked items.
    pub checked: BTreeSet<usize>,
    pub answer: String,
}

/// What is entered in a form, tab by tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormValues {
    pub tabs: Vec<TabValues>,
}

impl Form {
    /// Values with nothing entered.
    pub fn empty_values(&self) -> FormValues {
        FormValues {
            tabs: vec![TabValues::default(); self.tabs.len()],
        }
    }

    /// Whether tab `tab` shows its checklist with `values`.
    pub fn checklist_shown(&self, tab: usize, values: &TabValues) -> bool {
        values.choice == Some(self.tabs[tab].checklist_on)
    }

    /// Whether tab `tab` shows its question and answer with `values`.
    pub fn answer_shown(&self, tab: usize, values: &TabValues) -> bool {
        self.checklist_shown(tab, values) && !values.checked.is_empty()
    }

    /// The question of tab `tab` with the names of the items checked in `values`.
    pub fn question(&self, tab: usize, values: &TabValues) -> String {
        let tab = &self.tabs[tab];
        let names: Vec<&str> = values
            .checked
            .iter()
            .filter_map(|&item| tab.items.get(item).map(String::as_str))
            .collect();
        tab.question.replace(ITEMS, &names.join(", "))
    }

    /// The tabs without a choice in `values`.
    pub fn without_choice(&self, values: &FormValues) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&tab| values.tabs[tab].choice.is_none())
            .collect()
    }

    /// The tabs that show an answer in `values` and leave it blank.
    pub fn with_blank_answer(&self, values: &FormValues) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&tab| {
                let entered = &values.tabs[tab];
                self.answer_shown(tab, entered) && entered.answer.trim().is_empty()
            })
            .collect()
    }
}

/// A key the form acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Left,
    Right,
    Up,
    Down,
    Tab,
    BackTab,
    Enter,
    Space,
    Backspace,
    Delete,
    Home,
    End,
    Char(char),
    /// Ctrl-C or Ctrl-D.
    Interrupt,
}

/// What a key did to the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Continue,
    Submit,
    Interrupt,
}

/// What the focus is on within a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Choice(usize),
    Item(usize),
    Answer,
}

/// A form being filled in.
#[derive(Debug)]
pub(crate) struct FormState<'a> {
    form: &'a Form,
    values: FormValues,
    /// The open tab; `form.tabs.len()` is the submit tab.
    tab: usize,
    focus: Focus,
    /// The cursor in the answer, in chars.
    cursor: usize,
}

impl<'a> FormState<'a> {
    /// The form opened on its first tab with `initial`, or with nothing entered. Initial values
    /// for tabs the form does not have are ignored, and tabs without one start empty.
    pub(crate) fn new(form: &'a Form, initial: Option<&FormValues>) -> Self {
        let mut values = form.empty_values();
        if let Some(initial) = initial {
            for (value, given) in values.tabs.iter_mut().zip(&initial.tabs) {
                value.clone_from(given);
            }
        }
        let mut state = Self {
            form,
            values,
            tab: 0,
            focus: Focus::Choice(0),
            cursor: 0,
        };
        state.open(0);
        state
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> &FormValues {
        &self.values
    }

    pub(crate) fn into_values(self) -> FormValues {
        self.values
    }

    #[cfg(test)]
    pub(crate) fn tab(&self) -> usize {
        self.tab
    }

    #[cfg(test)]
    pub(crate) fn focus(&self) -> Focus {
        self.focus
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    fn on_submit_tab(&self) -> bool {
        self.tab == self.form.tabs.len()
    }

    /// Opens `tab`, with the focus on its choice (or its first choice) and the cursor at the end
    /// of its answer.
    fn open(&mut self, tab: usize) {
        self.tab = tab;
        if let Some(values) = self.values.tabs.get(tab) {
            self.focus = Focus::Choice(values.choice.unwrap_or(0));
            self.cursor = values.answer.chars().count();
        }
    }

    /// What the focus can be on in the open tab, top to bottom.
    fn focuses(&self) -> Vec<Focus> {
        let Some(values) = self.values.tabs.get(self.tab) else {
            return Vec::new();
        };
        let tab = &self.form.tabs[self.tab];
        let mut focuses: Vec<Focus> = (0..tab.choices.len()).map(Focus::Choice).collect();
        if self.form.checklist_shown(self.tab, values) {
            focuses.extend((0..tab.items.len()).map(Focus::Item));
            if self.form.answer_shown(self.tab, values) {
                focuses.push(Focus::Answer);
            }
        }
        focuses
    }

    /// Acts on `key`.
    pub(crate) fn press(&mut self, key: Key) -> Outcome {
        let in_answer = !self.on_submit_tab() && self.focus == Focus::Answer;
        match key {
            Key::Interrupt => return Outcome::Interrupt,
            Key::Tab => self.next_tab(),
            Key::BackTab => self.previous_tab(),
            Key::Left if in_answer => self.cursor = self.cursor.saturating_sub(1),
            Key::Right if in_answer => self.cursor = (self.cursor + 1).min(self.answer_len()),
            Key::Left => self.previous_tab(),
            Key::Right => self.next_tab(),
            Key::Up => self.move_focus(-1),
            Key::Down => self.move_focus(1),
            Key::Enter if self.on_submit_tab() => {
                if self.form.without_choice(&self.values).is_empty() {
                    return Outcome::Submit;
                }
            }
            Key::Enter if in_answer => self.next_tab(),
            Key::Enter | Key::Space if !in_answer => self.choose(),
            Key::Space => self.insert(' '),
            Key::Char(c) if in_answer => self.insert(c),
            Key::Backspace if in_answer && self.cursor > 0 => {
                self.cursor -= 1;
                self.remove_at_cursor();
            }
            Key::Delete if in_answer && self.cursor < self.answer_len() => self.remove_at_cursor(),
            Key::Home if in_answer => self.cursor = 0,
            Key::End if in_answer => self.cursor = self.answer_len(),
            _ => {}
        }
        Outcome::Continue
    }

    fn next_tab(&mut self) {
        if !self.on_submit_tab() {
            self.open(self.tab + 1);
        }
    }

    fn previous_tab(&mut self) {
        if self.tab > 0 {
            self.open(self.tab - 1);
        }
    }

    fn move_focus(&mut self, by: isize) {
        let focuses = self.focuses();
        let Some(at) = focuses.iter().position(|&focus| focus == self.focus) else {
            return;
        };
        if let Some(&focus) = at.checked_add_signed(by).and_then(|at| focuses.get(at)) {
            self.focus = focus;
        }
    }

    /// Chooses the focused choice or toggles the focused item.
    fn choose(&mut self) {
        let Some(values) = self.values.tabs.get_mut(self.tab) else {
            return;
        };
        match self.focus {
            Focus::Choice(choice) => values.choice = Some(choice),
            Focus::Item(item) => {
                if !values.checked.remove(&item) {
                    values.checked.insert(item);
                }
            }
            Focus::Answer => {}
        }
    }

    fn answer_len(&self) -> usize {
        self.values.tabs[self.tab].answer.chars().count()
    }

    fn insert(&mut self, c: char) {
        let answer = &mut self.values.tabs[self.tab].answer;
        answer.insert(byte_index(answer, self.cursor), c);
        self.cursor += 1;
    }

    fn remove_at_cursor(&mut self) {
        let answer = &mut self.values.tabs[self.tab].answer;
        answer.remove(byte_index(answer, self.cursor));
    }
}

/// The byte index of the char at `chars` in `text`, or its length past the last char.
fn byte_index(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(index, _)| index)
}

/// A form laid out for a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Screen {
    pub lines: Vec<String>,
    /// Where the cursor goes (row, column), when the answer has the focus.
    pub cursor: Option<(usize, usize)>,
}

/// Lays `state` out in lines of at most `width` columns.
pub(crate) fn render(state: &FormState<'_>, width: usize) -> Screen {
    let width = width.max(8);
    let form = state.form;
    let mut lines = tab_row(form, state.tab, width);
    lines.push(String::new());
    let mut cursor = None;

    match (form.tabs.get(state.tab), state.values.tabs.get(state.tab)) {
        (Some(tab), Some(values)) => {
            lines.extend(wrap(&tab.prompt, width));
            for (index, choice) in tab.choices.iter().enumerate() {
                let mark = if values.choice == Some(index) {
                    "(*)"
                } else {
                    "( )"
                };
                let line = format!(
                    "{} {mark} {choice}",
                    pointer(state.focus == Focus::Choice(index))
                );
                lines.extend(wrap(&line, width));
            }
            if form.checklist_shown(state.tab, values) {
                lines.push(String::new());
                lines.extend(wrap(&tab.checklist_prompt, width));
                for (index, item) in tab.items.iter().enumerate() {
                    let mark = if values.checked.contains(&index) {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    let line = format!(
                        "{} {mark} {item}",
                        pointer(state.focus == Focus::Item(index))
                    );
                    lines.extend(wrap(&line, width));
                }
            }
            if form.answer_shown(state.tab, values) {
                lines.push(String::new());
                lines.extend(wrap(&form.question(state.tab, values), width));
                for &item in &values.checked {
                    for line in tab.material.get(item).into_iter().flatten() {
                        lines.extend(wrap(&format!("  {line}"), width));
                    }
                }
                lines.push(String::new());
                let focused = state.focus == Focus::Answer;
                let (answer, (row, column)) =
                    wrap_with_cursor(&values.answer, state.cursor, width - 2);
                if focused {
                    cursor = Some((lines.len() + row, column + 2));
                }
                for (index, line) in answer.into_iter().enumerate() {
                    let head = if index == 0 { pointer(focused) } else { " " };
                    lines.push(format!("{head} {line}"));
                }
            }
        }
        _ => {
            let without = form.without_choice(&state.values);
            if !without.is_empty() {
                lines.extend(wrap(&form.text.no_choice, width));
                for &tab in &without {
                    lines.extend(wrap(&format!("  - {}", form.tabs[tab].title), width));
                }
            }
            let blank = form.with_blank_answer(&state.values);
            if !blank.is_empty() {
                lines.extend(wrap(&form.text.blank_answer, width));
                for &tab in &blank {
                    lines.extend(wrap(&format!("  - {}", form.tabs[tab].title), width));
                }
            }
            let next = if without.is_empty() {
                &form.text.ready
            } else {
                &form.text.not_ready
            };
            lines.extend(wrap(next, width));
        }
    }

    lines.push(String::new());
    lines.extend(wrap(&form.text.guide, width));
    Screen { lines, cursor }
}

fn pointer(focused: bool) -> &'static str {
    if focused { ">" } else { " " }
}

/// The row of tabs, the open one in brackets, wrapped at `width`.
fn tab_row(form: &Form, open: usize, width: usize) -> Vec<String> {
    let titles = form
        .tabs
        .iter()
        .map(|tab| tab.title.as_str())
        .chain([form.text.submit_title.as_str()]);
    let mut lines = vec![String::new()];
    for (index, title) in titles.enumerate() {
        let label = if index == open {
            format!("[{title}]")
        } else {
            format!(" {title} ")
        };
        let line = lines.last_mut().unwrap();
        if !line.is_empty() && text_width(line) + 1 + text_width(&label) > width {
            lines.push(label);
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&label);
        }
    }
    lines
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// `text` cut into lines of at most `width` columns, counting a full-width char as 2. An empty
/// text is one empty line.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    wrap_with_cursor(text, 0, width).0
}

/// [`wrap`], and the row and column of the cursor at char `cursor`. A cursor past a full last
/// line goes to the start of the next row.
pub(crate) fn wrap_with_cursor(
    text: &str,
    cursor: usize,
    width: usize,
) -> (Vec<String>, (usize, usize)) {
    let width = width.max(2);
    let mut lines = vec![String::new()];
    let mut column = 0;
    let mut at = (0, 0);
    for (index, c) in text.chars().enumerate() {
        let w = char_width(c);
        if column + w > width {
            lines.push(String::new());
            column = 0;
        }
        if index == cursor {
            at = (lines.len() - 1, column);
        }
        lines.last_mut().unwrap().push(c);
        column += w;
    }
    if cursor >= text.chars().count() {
        at = if column >= width {
            (lines.len(), 0)
        } else {
            (lines.len() - 1, column)
        };
    }
    (lines, at)
}
