//! A [`Ui`] that answers from a script: the user's inputs, choices, approvals and interrupts in
//! order (PRD §8.6 `--ui-script`, §9).
//!
//! The script file is toml with one array, `steps`. Each step answers the next question molt
//! asks, whichever kind it is:
//!
//! ```toml
//! steps = [
//!   { input = "add a function that sums a slice" },  # a line at the prompt
//!   { confirm = true },                              # yes (or false: no) to an approval
//!   { select = 2 },                                  # the 2nd item of a choice (from 1)
//!   "interrupt",                                     # Ctrl-C at any question
//!   "eof",                                           # Ctrl-D at the prompt
//!   { form = [                                       # a form submitted, tab by tab:
//!     { choice = 1, checked = [1, 3], answer = "…" }, # the choice, the checked items and the
//!     { choice = 3 },                                #   answer, numbered from 1
//!   ] },
//! ]
//! ```
//!
//! A form step gives what the tabs hold when submitted, not the keys: moving between tabs is the
//! form's own state (PRD §8.6). Checks and an answer under a choice that hides the checklist are
//! allowed, as the form keeps them.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::form::{Form, FormValues, TabValues};
use super::{Input, Ui, UiError};

/// One answer in a script.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// A line typed at the prompt.
    Input(String),
    /// The item chosen, numbered from 1 as listed.
    Select(usize),
    /// The answer to a yes/no question.
    Confirm(bool),
    /// Ctrl-C, at any kind of question.
    Interrupt,
    /// Ctrl-D at the prompt.
    Eof,
    /// A form submitted: what each tab holds.
    Form(Vec<FormTabStep>),
}

/// What one tab of a form holds in a script, numbered from 1.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormTabStep {
    pub choice: usize,
    #[serde(default)]
    pub checked: Vec<usize>,
    #[serde(default)]
    pub answer: String,
}

/// A form molt asked to be filled in, as it was passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedForm {
    pub form: Form,
    pub initial: Option<FormValues>,
}

impl Step {
    /// What kind of answer this is, for error messages.
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Step::Input(_) => "an input",
            Step::Select(_) => "a selection",
            Step::Confirm(_) => "a confirmation",
            Step::Interrupt => "an interrupt",
            Step::Eof => "an eof",
            Step::Form(_) => "a form",
        }
    }
}

/// The shape of a script file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptFile {
    steps: Vec<Step>,
}

/// Why a script file could not be read.
#[derive(Debug)]
pub enum ScriptError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptError::Io { path, source } => {
                write!(f, "cannot read the ui script {}: {source}", path.display())
            }
            ScriptError::Parse { path, source } => {
                write!(f, "invalid ui script {}: {source}", path.display())
            }
        }
    }
}

impl Error for ScriptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ScriptError::Io { source, .. } => Some(source),
            ScriptError::Parse { source, .. } => Some(source),
        }
    }
}

/// Answers each question with the next step of a script, and records what molt showed and asked.
#[derive(Debug)]
pub struct ScriptedUi {
    steps: VecDeque<Step>,
    shown: Vec<String>,
    asked: Vec<String>,
    forms: Vec<RecordedForm>,
    echo: bool,
    /// Whether the model's text is taken a line at a time, as a terminal shows it.
    streaming: bool,
    statuses: Vec<Option<String>>,
}

impl ScriptedUi {
    /// A script of `steps` that records what is shown without printing it.
    pub fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            shown: Vec::new(),
            asked: Vec::new(),
            forms: Vec::new(),
            echo: false,
            streaming: false,
            statuses: Vec::new(),
        }
    }

    /// Takes the model's text a line at a time as a terminal does ([`Ui::streams`]), each line
    /// recorded as shown on its own.
    #[cfg(test)]
    pub fn streaming(mut self) -> Self {
        self.streaming = true;
        self
    }

    /// What molt said it was doing while the user waited ([`Ui::status`]), in order.
    #[cfg(test)]
    pub fn statuses(&self) -> &[Option<String>] {
        &self.statuses
    }

    /// Reads the script file at `path` (the format is in the module documentation).
    pub fn load(path: &Path) -> Result<Self, ScriptError> {
        let text = fs::read_to_string(path).map_err(|source| ScriptError::Io {
            path: path.to_owned(),
            source,
        })?;
        let file: ScriptFile = toml::from_str(&text).map_err(|source| ScriptError::Parse {
            path: path.to_owned(),
            source,
        })?;
        Ok(Self::new(file.steps))
    }

    /// Also prints to stdout what is shown, each question and the scripted answer, so a run
    /// reads like a terminal session.
    pub fn echo_to_stdout(mut self) -> Self {
        self.echo = true;
        self
    }

    /// The texts passed to [`Ui::show`], in order.
    pub fn shown(&self) -> &[String] {
        &self.shown
    }

    /// The prompts of the questions answered or attempted, in order. A form's is the name of its
    /// submit tab; the form itself is in [`ScriptedUi::forms`].
    pub fn asked(&self) -> &[String] {
        &self.asked
    }

    /// The forms passed to [`Ui::form`], with their initial values, in order.
    pub fn forms(&self) -> &[RecordedForm] {
        &self.forms
    }

    fn print(&self, text: &str) {
        if self.echo {
            println!("{text}");
        }
    }

    fn next(&mut self, asked: &'static str, prompt: &str) -> Result<Step, UiError> {
        self.asked.push(prompt.to_owned());
        self.steps
            .pop_front()
            .ok_or(UiError::ScriptExhausted { asked })
    }
}

impl Ui for ScriptedUi {
    fn read_line(&mut self, prompt: &str) -> Result<Input, UiError> {
        let (input, echoed) = match self.next("an input", prompt)? {
            Step::Input(line) => (Input::Line(line.clone()), line),
            Step::Interrupt => (Input::Interrupted, "^C".to_owned()),
            Step::Eof => (Input::Eof, "^D".to_owned()),
            step => {
                return Err(UiError::ScriptMismatch {
                    asked: "an input",
                    step,
                });
            }
        };
        self.print(&format!("{prompt}{echoed}"));
        Ok(input)
    }

    fn show(&mut self, text: &str) {
        self.print(text);
        self.shown.push(text.to_owned());
    }

    fn status(&mut self, doing: Option<&str>) {
        self.statuses.push(doing.map(str::to_owned));
    }

    fn streams(&self) -> bool {
        self.streaming
    }

    fn stream_line(&mut self, line: &str, indent: &str) {
        self.show(&super::indented(&super::printable(line), indent));
    }

    fn select(&mut self, prompt: &str, items: &[&str]) -> Result<Option<usize>, UiError> {
        let step = self.next("a selection", prompt)?;
        self.print(prompt);
        for (index, item) in items.iter().enumerate() {
            self.print(&format!("  {}) {item}", index + 1));
        }
        match step {
            Step::Select(number) if (1..=items.len()).contains(&number) => {
                self.print(&format!("> {number}"));
                Ok(Some(number - 1))
            }
            Step::Select(number) => Err(UiError::SelectOutOfRange {
                number,
                items: items.len(),
            }),
            Step::Interrupt => {
                self.print("> ^C");
                Ok(None)
            }
            step => Err(UiError::ScriptMismatch {
                asked: "a selection",
                step,
            }),
        }
    }

    fn confirm(&mut self, prompt: &str) -> Result<Option<bool>, UiError> {
        let answer = match self.next("a confirmation", prompt)? {
            Step::Confirm(answer) => Some(answer),
            Step::Interrupt => None,
            step => {
                return Err(UiError::ScriptMismatch {
                    asked: "a confirmation",
                    step,
                });
            }
        };
        let echoed = match answer {
            Some(true) => "yes",
            Some(false) => "no",
            None => "^C",
        };
        self.print(&format!("{prompt} [y/n] {echoed}"));
        Ok(answer)
    }

    fn form(
        &mut self,
        form: &Form,
        initial: Option<&FormValues>,
    ) -> Result<Option<FormValues>, UiError> {
        self.forms.push(RecordedForm {
            form: form.clone(),
            initial: initial.cloned(),
        });
        let tabs = match self.next("a form", &form.text.submit_title)? {
            Step::Form(tabs) => tabs,
            Step::Interrupt => {
                self.print("form: ^C");
                return Ok(None);
            }
            step => {
                return Err(UiError::ScriptMismatch {
                    asked: "a form",
                    step,
                });
            }
        };
        let values = form_values(form, &tabs)?;
        for (tab, value) in form.tabs.iter().zip(&values.tabs) {
            let choice = value
                .choice
                .map_or("", |choice| tab.choices[choice].as_str());
            let checked: Vec<&str> = value
                .checked
                .iter()
                .map(|&item| tab.items[item].as_str())
                .collect();
            self.print(&format!(
                "{}: {choice}; checked: {}; answer: {}",
                tab.title,
                checked.join(", "),
                value.answer
            ));
        }
        Ok(Some(values))
    }
}

/// The values the script's `tabs` give `form`.
fn form_values(form: &Form, tabs: &[FormTabStep]) -> Result<FormValues, UiError> {
    if tabs.len() != form.tabs.len() {
        return Err(UiError::FormTabCount {
            tabs: form.tabs.len(),
            script: tabs.len(),
        });
    }
    let index = |tab: usize, what: &'static str, number: usize, items: usize| {
        if (1..=items).contains(&number) {
            Ok(number - 1)
        } else {
            Err(UiError::FormOutOfRange {
                tab: tab + 1,
                what,
                number,
                items,
            })
        }
    };
    let mut values = Vec::with_capacity(tabs.len());
    for (at, (tab, step)) in form.tabs.iter().zip(tabs).enumerate() {
        let choice = index(at, "choice", step.choice, tab.choices.len())?;
        let checked = step
            .checked
            .iter()
            .map(|&number| index(at, "item", number, tab.items.len()))
            .collect::<Result<_, _>>()?;
        values.push(TabValues {
            choice: Some(choice),
            checked,
            answer: step.answer.clone(),
        });
    }
    Ok(FormValues { tabs: values })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{FormText, Tab};

    fn parse(text: &str) -> Result<Vec<Step>, toml::de::Error> {
        toml::from_str::<ScriptFile>(text).map(|file| file.steps)
    }

    #[test]
    fn a_script_file_lists_every_kind_of_step() {
        let steps = parse(
            r#"steps = [
                { input = "hello" },
                { select = 2 },
                { confirm = true },
                "interrupt",
                "eof",
            ]"#,
        )
        .unwrap();
        assert_eq!(
            steps,
            [
                Step::Input("hello".to_owned()),
                Step::Select(2),
                Step::Confirm(true),
                Step::Interrupt,
                Step::Eof,
            ]
        );
    }

    #[test]
    fn a_script_file_also_takes_steps_as_array_tables() {
        let steps = parse("[[steps]]\ninput = \"/quit\"\n\n[[steps]]\nconfirm = false\n").unwrap();
        assert_eq!(
            steps,
            [Step::Input("/quit".to_owned()), Step::Confirm(false)]
        );
    }

    #[test]
    fn a_script_file_with_an_unknown_step_is_rejected() {
        assert!(parse(r#"steps = [{ answer = "yes" }]"#).is_err());
    }

    #[test]
    fn a_script_file_with_an_unknown_key_is_rejected() {
        assert!(parse("steps = []\nresponses = []\n").is_err());
    }

    #[test]
    fn load_reports_a_missing_file_with_its_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("missing.toml");
        let error = ScriptedUi::load(&path).unwrap_err();
        assert!(matches!(&error, ScriptError::Io { path: p, .. } if *p == path));
    }

    #[test]
    fn inputs_interrupts_and_eof_answer_the_prompt_in_order() {
        let mut ui = ScriptedUi::new([Step::Input("hi".to_owned()), Step::Interrupt, Step::Eof]);
        assert_eq!(
            ui.read_line("molt> ").unwrap(),
            Input::Line("hi".to_owned())
        );
        assert_eq!(ui.read_line("molt> ").unwrap(), Input::Interrupted);
        assert_eq!(ui.read_line("molt> ").unwrap(), Input::Eof);
    }

    #[test]
    fn select_returns_the_index_of_the_item_numbered_from_1() {
        let mut ui = ScriptedUi::new([Step::Select(3)]);
        assert_eq!(ui.select("Choose", &["a", "b", "c"]).unwrap(), Some(2));
    }

    #[test]
    fn select_rejects_a_number_outside_the_items() {
        for number in [0, 4] {
            let mut ui = ScriptedUi::new([Step::Select(number)]);
            assert!(matches!(
                ui.select("Choose", &["a", "b", "c"]),
                Err(UiError::SelectOutOfRange { number: n, items: 3 }) if n == number
            ));
        }
    }

    #[test]
    fn an_interrupt_answers_a_selection_and_a_confirmation_with_none() {
        let mut ui = ScriptedUi::new([Step::Interrupt, Step::Interrupt]);
        assert_eq!(ui.select("Choose", &["a"]).unwrap(), None);
        assert_eq!(ui.confirm("Go?").unwrap(), None);
    }

    #[test]
    fn confirm_returns_the_scripted_answer() {
        let mut ui = ScriptedUi::new([Step::Confirm(true), Step::Confirm(false)]);
        assert_eq!(ui.confirm("Go?").unwrap(), Some(true));
        assert_eq!(ui.confirm("Go?").unwrap(), Some(false));
    }

    #[test]
    fn a_step_of_another_kind_is_an_error_naming_both() {
        let mut ui = ScriptedUi::new([Step::Confirm(true)]);
        let error = ui.read_line("molt> ").unwrap_err();
        assert_eq!(
            error.to_string(),
            "the ui script was asked for an input, but its next step is a confirmation"
        );
    }

    #[test]
    fn eof_does_not_answer_a_confirmation() {
        let mut ui = ScriptedUi::new([Step::Eof]);
        assert!(matches!(
            ui.confirm("Go?"),
            Err(UiError::ScriptMismatch {
                step: Step::Eof,
                ..
            })
        ));
    }

    #[test]
    fn running_out_of_steps_is_an_error() {
        let mut ui = ScriptedUi::new([]);
        assert!(matches!(
            ui.read_line("molt> "),
            Err(UiError::ScriptExhausted { asked: "an input" })
        ));
    }

    /// A form of 2 tabs, each with choices `a`, `b` (`a` shows the checklist) and items `x`, `y`.
    fn form() -> Form {
        let tab = |title: &str| Tab {
            title: title.to_owned(),
            prompt: "?".to_owned(),
            choices: vec!["a".to_owned(), "b".to_owned()],
            checklist_on: 0,
            checklist_prompt: "?".to_owned(),
            items: vec!["x".to_owned(), "y".to_owned()],
            question: "?".to_owned(),
            material: Vec::new(),
        };
        Form {
            tabs: vec![tab("one"), tab("two")],
            text: FormText {
                submit_title: "Submit".to_owned(),
                guide: String::new(),
                no_choice: String::new(),
                blank_answer: String::new(),
                ready: String::new(),
                not_ready: String::new(),
            },
        }
    }

    #[test]
    fn a_script_file_takes_a_form_tab_by_tab() {
        let steps = parse(
            r#"steps = [{ form = [{ choice = 1, checked = [2], answer = "hi" }, { choice = 2 }] }]"#,
        )
        .unwrap();
        assert_eq!(
            steps,
            [Step::Form(vec![
                FormTabStep {
                    choice: 1,
                    checked: vec![2],
                    answer: "hi".to_owned(),
                },
                FormTabStep {
                    choice: 2,
                    checked: Vec::new(),
                    answer: String::new(),
                },
            ])]
        );
    }

    #[test]
    fn a_form_tab_without_a_choice_is_rejected() {
        assert!(parse(r#"steps = [{ form = [{ checked = [1] }] }]"#).is_err());
    }

    #[test]
    fn a_form_step_returns_the_values_it_gives_numbered_from_0() {
        let mut ui = ScriptedUi::new([Step::Form(vec![
            FormTabStep {
                choice: 1,
                checked: vec![2, 1],
                answer: "hi".to_owned(),
            },
            // Checks under a choice that hides the checklist are kept, as the form keeps them.
            FormTabStep {
                choice: 2,
                checked: vec![1],
                answer: "kept".to_owned(),
            },
        ])]);
        let values = ui.form(&form(), None).unwrap().unwrap();
        assert_eq!(
            values.tabs,
            [
                TabValues {
                    choice: Some(0),
                    checked: [0, 1].into(),
                    answer: "hi".to_owned(),
                },
                TabValues {
                    choice: Some(1),
                    checked: [0].into(),
                    answer: "kept".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn an_interrupt_answers_a_form_with_none() {
        let mut ui = ScriptedUi::new([Step::Interrupt]);
        assert_eq!(ui.form(&form(), None).unwrap(), None);
    }

    #[test]
    fn a_form_records_the_form_and_its_initial_values() {
        let initial = FormValues {
            tabs: vec![TabValues::default(), TabValues::default()],
        };
        let mut ui = ScriptedUi::new([Step::Interrupt]);
        ui.form(&form(), Some(&initial)).unwrap();
        assert_eq!(
            ui.forms(),
            [RecordedForm {
                form: form(),
                initial: Some(initial),
            }]
        );
    }

    #[test]
    fn a_form_step_with_another_number_of_tabs_is_an_error() {
        let mut ui = ScriptedUi::new([Step::Form(vec![FormTabStep {
            choice: 1,
            checked: Vec::new(),
            answer: String::new(),
        }])]);
        assert!(matches!(
            ui.form(&form(), None),
            Err(UiError::FormTabCount { tabs: 2, script: 1 })
        ));
    }

    #[test]
    fn a_form_step_choosing_or_checking_outside_the_tab_is_an_error() {
        let tab = |choice: usize, checked: Vec<usize>| FormTabStep {
            choice,
            checked,
            answer: String::new(),
        };
        let mut ui = ScriptedUi::new([
            Step::Form(vec![tab(1, Vec::new()), tab(3, Vec::new())]),
            Step::Form(vec![tab(1, vec![0]), tab(1, Vec::new())]),
        ]);
        assert!(matches!(
            ui.form(&form(), None),
            Err(UiError::FormOutOfRange {
                tab: 2,
                what: "choice",
                number: 3,
                items: 2
            })
        ));
        assert!(matches!(
            ui.form(&form(), None),
            Err(UiError::FormOutOfRange {
                tab: 1,
                what: "item",
                number: 0,
                items: 2
            })
        ));
    }

    #[test]
    fn a_step_of_another_kind_does_not_answer_a_form() {
        let mut ui = ScriptedUi::new([Step::Select(1)]);
        assert!(matches!(
            ui.form(&form(), None),
            Err(UiError::ScriptMismatch {
                asked: "a form",
                step: Step::Select(1)
            })
        ));
    }

    #[test]
    fn shown_records_each_text_in_order() {
        let mut ui = ScriptedUi::new([]);
        ui.show("one");
        ui.show("two");
        assert_eq!(ui.shown(), ["one", "two"]);
    }
}
