//! The model's Markdown, a line at a time (PRD §8.1): rendered for a terminal, the code blocks of
//! the main line folded, and the first code block of an explanation held back for its
//! comparison. Text streams in by lines, so each piece keeps only what it needs between lines.

/// A line of the model's text, rendered for a terminal or written as it came.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Styled {
    /// A heading, `##` and all removed.
    Heading(String),
    /// A line of a code block, or a fence line (`None` for the fence itself).
    Code(Option<String>),
    /// Prose, in spans.
    Prose(Vec<Span>),
}

/// A part of a line of prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Plain(String),
    Bold(String),
    Italic(String),
    Code(String),
}

/// Renders the model's Markdown line by line: headings, emphasis, inline code, code blocks and
/// lists. A line is rendered once it is complete; a code block is known by its fences.
#[derive(Debug, Default)]
pub struct Renderer {
    blocks: Blocks,
}

impl Renderer {
    /// What `line` is, given the lines before it.
    pub fn line(&mut self, line: &str) -> Styled {
        match self.blocks.line(line) {
            Line::Opens | Line::Closes => return Styled::Code(None),
            Line::Code => return Styled::Code(Some(line.to_owned())),
            Line::Text => {}
        }
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            return Styled::Heading(trimmed[hashes..].trim().to_owned());
        }
        // A list item keeps its marker, as `•` for `-`, `*` and `+`.
        let indent = &line[..line.len() - trimmed.len()];
        let (marker, rest) = match trimmed.split_at_checked(2) {
            Some(("- " | "* " | "+ ", rest)) => (format!("{indent}• "), rest),
            _ => (indent.to_owned(), trimmed),
        };
        let mut spans = spans(rest);
        if !marker.is_empty() {
            spans.insert(0, Span::Plain(marker));
        }
        Styled::Prose(spans)
    }
}

/// The fence that opened a code block: its character and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fence {
    marker: char,
    length: usize,
}

impl Fence {
    /// The fence `line` opens a code block with: 3 or more of `` ` `` or `~`, an info string
    /// (`rust`) allowed after them.
    pub fn opened_by(line: &str) -> Option<Fence> {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let length = trimmed.chars().take_while(|&c| c == marker).count();
        (length >= 3).then_some(Fence { marker, length })
    }

    /// Whether `line` closes the block this fence opened: the same character, at least as many,
    /// and nothing after them.
    pub fn closed_by(self, line: &str) -> bool {
        let trimmed = line.trim();
        let length = trimmed.chars().take_while(|&c| c == self.marker).count();
        length >= self.length && trimmed.chars().count() == length
    }
}

/// Tracks the code blocks of a text line by line.
#[derive(Debug, Default, Clone, Copy)]
pub struct Blocks {
    open: Option<Fence>,
}

/// What a line is to [`Blocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// Outside any code block.
    Text,
    /// The fence that opens a code block.
    Opens,
    /// A line inside a code block.
    Code,
    /// The fence that closes the code block.
    Closes,
}

impl Blocks {
    /// What `line` is, given the lines before it.
    pub fn line(&mut self, line: &str) -> Line {
        match self.open {
            Some(fence) if fence.closed_by(line) => {
                self.open = None;
                Line::Closes
            }
            Some(_) => Line::Code,
            None => match Fence::opened_by(line) {
                Some(fence) => {
                    self.open = Some(fence);
                    Line::Opens
                }
                None => Line::Text,
            },
        }
    }

    /// Whether a code block is open.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

/// The spans of a line of prose: `**bold**`, `*italic*` or `_italic_`, and `` `code` ``, much as
/// CommonMark reads them: no space just inside the markers, so that `2 * 3` stays as it is, and
/// `_` not inside a word, so that `snake_case_name` does, while `*` may be (`これは**重要**です`).
/// A marker not closed on the line stays as it is.
fn spans(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut at = 0;
    while at < chars.len() {
        match emphasis(&chars, at) {
            Some((span, next)) => {
                if !plain.is_empty() {
                    spans.push(Span::Plain(std::mem::take(&mut plain)));
                }
                spans.push(span);
                at = next;
            }
            None => {
                plain.push(chars[at]);
                at += 1;
            }
        }
    }
    if !plain.is_empty() {
        spans.push(Span::Plain(plain));
    }
    spans
}

/// The span that opens at `at` in `chars`, and where the text after it starts.
fn emphasis(chars: &[char], at: usize) -> Option<(Span, usize)> {
    let starts = |marker: &str| {
        marker
            .chars()
            .enumerate()
            .all(|(i, c)| chars.get(at + i) == Some(&c))
    };
    let marker = ["`", "**", "*", "_"].into_iter().find(|m| starts(m))?;
    let width = marker.chars().count();
    let first = marker.chars().next()?;
    let inner_start = at + width;
    let word = |i: usize| chars.get(i).is_some_and(|c| c.is_alphanumeric());
    if marker != "`" {
        // Emphasis opens before a character that is not a space, and `_` not inside a word.
        if chars.get(inner_start).is_none_or(|c| c.is_whitespace())
            || (first == '_' && at > 0 && word(at - 1))
        {
            return None;
        }
    }
    let mut end = inner_start + 1;
    while end + width <= chars.len() {
        let closes = (0..width).all(|i| chars[end + i] == first)
            && (marker == "`"
                || (!chars[end - 1].is_whitespace() && !(first == '_' && word(end + width))));
        if closes && end > inner_start {
            let inner: String = chars[inner_start..end].iter().collect();
            let span = match marker {
                "`" => Span::Code(inner),
                "**" => Span::Bold(inner),
                _ => Span::Italic(inner),
            };
            return Some((span, end + width));
        }
        end += 1;
    }
    None
}

/// Folds each code block of the main line's text into one line (PRD §8.1): code reaches the user
/// only as a step or a diff. Lines go in one at a time; what to show comes out.
#[derive(Debug, Default)]
pub struct Folder {
    blocks: Blocks,
    /// The lines of the code block open so far.
    lines: usize,
}

impl Folder {
    /// What to show for `line`: itself outside a code block, nothing inside one, and the folded
    /// line when the block closes.
    pub fn line(&mut self, line: &str) -> Option<String> {
        match self.blocks.line(line) {
            Line::Text => Some(line.to_owned()),
            Line::Opens => {
                self.lines = 0;
                None
            }
            Line::Code => {
                self.lines += 1;
                None
            }
            Line::Closes => Some(folded(self.lines)),
        }
    }

    /// The end of the text: a code block not closed is folded to here.
    pub fn end(&mut self) -> Option<String> {
        let open = self.blocks.is_open();
        self.blocks = Blocks::default();
        open.then(|| folded(self.lines))
    }
}

/// `[code: 12 lines]`, `[code: 1 line]`.
fn folded(lines: usize) -> String {
    if lines == 1 {
        "[code: 1 line]".to_owned()
    } else {
        format!("[code: {lines} lines]")
    }
}

/// Folds the code blocks of a whole text, as [`Folder`] does line by line.
pub fn fold(text: &str) -> String {
    let mut folder = Folder::default();
    let mut lines: Vec<String> = text.lines().filter_map(|line| folder.line(line)).collect();
    lines.extend(folder.end());
    lines.join("\n")
}

/// Holds back the first code block of an explanation, the lines without the items, which the
/// comparison shows after it (PRD §8.6). Every other line passes.
#[derive(Debug, Default)]
pub struct HoldFirstCode {
    blocks: Blocks,
    /// Whether the first code block has closed.
    past: bool,
}

impl HoldFirstCode {
    /// Whether `line` is to be shown as it streams in.
    pub fn passes(&mut self, line: &str) -> bool {
        if self.past {
            return true;
        }
        match self.blocks.line(line) {
            Line::Text => true,
            Line::Opens | Line::Code => false,
            Line::Closes => {
                self.past = true;
                false
            }
        }
    }
}

#[cfg(test)]
mod tests;
