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
    in_code: bool,
}

impl Renderer {
    /// What `line` is, given the lines before it.
    pub fn line(&mut self, line: &str) -> Styled {
        if is_fence(line) {
            self.in_code = !self.in_code;
            return Styled::Code(None);
        }
        if self.in_code {
            return Styled::Code(Some(line.to_owned()));
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

/// Whether `line` opens or closes a code block.
pub fn is_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// The spans of a line of prose: `**bold**`, `*italic*` or `_italic_`, and `` `code` ``. A marker
/// not closed on the line stays as it is.
fn spans(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let found = [("`", 1), ("**", 2), ("*", 1), ("_", 1)]
            .into_iter()
            .find_map(|(marker, _)| {
                let after = rest.strip_prefix(marker)?;
                let end = after.find(marker)?;
                (end > 0).then(|| (marker, &after[..end], &after[end + marker.len()..]))
            });
        match found {
            Some((marker, inner, after)) => {
                if !plain.is_empty() {
                    spans.push(Span::Plain(std::mem::take(&mut plain)));
                }
                spans.push(match marker {
                    "`" => Span::Code(inner.to_owned()),
                    "**" => Span::Bold(inner.to_owned()),
                    _ => Span::Italic(inner.to_owned()),
                });
                rest = after;
            }
            None => {
                let mut chars = rest.chars();
                plain.push(chars.next().expect("not empty"));
                rest = chars.as_str();
            }
        }
    }
    if !plain.is_empty() {
        spans.push(Span::Plain(plain));
    }
    spans
}

/// Folds each code block of the main line's text into one line (PRD §8.1): code reaches the user
/// only as a step or a diff. Lines go in one at a time; what to show comes out.
#[derive(Debug, Default)]
pub struct Folder {
    /// The lines of the code block open, if one is.
    open: Option<usize>,
}

impl Folder {
    /// What to show for `line`: itself outside a code block, nothing inside one, and the folded
    /// line when the block closes.
    pub fn line(&mut self, line: &str) -> Option<String> {
        match (&mut self.open, is_fence(line)) {
            (None, true) => {
                self.open = Some(0);
                None
            }
            (None, false) => Some(line.to_owned()),
            (Some(count), false) => {
                *count += 1;
                None
            }
            (Some(count), true) => {
                let folded = folded(*count);
                self.open = None;
                Some(folded)
            }
        }
    }

    /// The end of the text: a code block not closed is folded to here.
    pub fn end(&mut self) -> Option<String> {
        self.open.take().map(folded)
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
    state: Hold,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum Hold {
    #[default]
    Before,
    Inside,
    After,
}

impl HoldFirstCode {
    /// Whether `line` is to be shown as it streams in.
    pub fn passes(&mut self, line: &str) -> bool {
        match self.state {
            Hold::Before if is_fence(line) => {
                self.state = Hold::Inside;
                false
            }
            Hold::Inside => {
                if is_fence(line) {
                    self.state = Hold::After;
                }
                false
            }
            Hold::Before | Hold::After => true,
        }
    }
}

#[cfg(test)]
mod tests;
