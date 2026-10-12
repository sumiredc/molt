//! The form of `molt init` (PRD UC1, §8.6): one tab per category with the report, the elements
//! the user has written, and a fixed question about them, in English or Japanese by the locale.
//!
//! The questions are not made by a model: each category has a fixed template the checked
//! elements' names are put in. With a reference repository, the places it uses each element are
//! listed under the question. The [`Ui`](crate::ui::Ui) draws the form; this module builds it and
//! reads the submitted values back.

#[cfg(test)]
mod tests;

use crate::profile::element::{Category, Element};
use crate::profile::store::Report;
use crate::repo_scan::RepoScan;
use crate::ui::{Form, FormText, FormValues, Tab};

/// The language of the form's strings (PRD §3 "Language", §8.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Japanese,
}

impl Language {
    /// Japanese when the first non-empty one of `LC_ALL`, `LC_MESSAGES` and `LANG` starts with
    /// `ja`, otherwise English. `var` reads an environment variable.
    pub fn from_locale(var: impl Fn(&str) -> Option<String>) -> Self {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(var)
            .find(|value| !value.is_empty());
        match locale {
            Some(locale) if locale.starts_with("ja") => Language::Japanese,
            _ => Language::English,
        }
    }

    /// [`Language::from_locale`] of this process's environment.
    pub fn from_env() -> Self {
        Self::from_locale(|name| std::env::var(name).ok())
    }
}

/// The reports offered on each tab, in the order of the choices.
const REPORTS: [Report; 3] = [Report::HaveWritten, Report::CanRead, Report::DontKnow];

/// The strings of the form in one language.
struct Strings {
    reports: [&'static str; 3],
    /// How well the user knows the category; `{category}` and `{elements}` are put in.
    prompt: &'static str,
    checklist_prompt: &'static str,
    /// The question of every category, with [`ITEMS`](crate::ui::ITEMS) where the checked
    /// elements' names go. It asks for what the user wrote and what they would have written
    /// without it, and names no idea of the category, so that the answer is not given away and
    /// it fits every element (a `for` loop as much as `Arc`).
    question: &'static str,
    /// Introduces the places a reference repository uses an element; `{element}` and `{places}`
    /// are put in.
    material: &'static str,
    text: [&'static str; 6],
}

const ENGLISH: Strings = Strings {
    reports: ["have written", "can read", "don't know"],
    prompt: "How well do you know {category}? ({elements})",
    checklist_prompt: "Check the ones you have written.",
    question: "Where have you used {items}? For each, show what you wrote, and what you would have written instead without it.",
    material: "`{element}` in your repository: {places}",
    text: [
        "Submit",
        "←/→ or Tab: tabs   ↑/↓: move   Space/Enter: choose   Enter in the answer: next tab   Ctrl-C: quit",
        "Not answered yet:",
        "Checked, but the answer is blank (these stay unknown):",
        "Press Enter to submit.",
        "Answer every tab to submit.",
    ],
};

const JAPANESE: Strings = Strings {
    reports: ["書いたことがある", "読める", "知らない"],
    prompt: "{category} をどのくらい知っていますか？（{elements}）",
    checklist_prompt: "書いたことがあるものにチェックしてください。",
    question: "{items} をどこで使いましたか？それぞれについて、書いたコードと、それを使わない場合の書き方を示してください。",
    material: "あなたのリポジトリの `{element}`: {places}",
    text: [
        "送信",
        "←/→・Tab: タブ移動　↑/↓: 移動　Space/Enter: 選ぶ　回答で Enter: 次のタブ　Ctrl-C: 中断",
        "まだ答えていないタブ:",
        "チェックしたが回答が空のタブ（unknown のままになります）:",
        "Enter で送信します。",
        "すべてのタブに答えると送信できます。",
    ],
};

/// The tab title of `category` in `language`.
fn title(category: Category, language: Language) -> &'static str {
    use Category::*;
    match language {
        Language::English => category.name(),
        Language::Japanese => match category {
            Ownership => "所有権",
            Borrowing => "借用",
            Lifetimes => "ライフタイム",
            TraitsGenerics => "トレイトとジェネリクス",
            SmartPointers => "スマートポインタ",
            ErrorHandling => "エラー処理",
            IteratorsClosures => "イテレータとクロージャ",
            Async => "非同期",
            Macros => "マクロ",
            Unsafe => "unsafe",
        },
    }
}

/// The form of `molt init` in `language`, with the places `scan` (if any) found for each element.
pub(crate) fn build(language: Language, scan: Option<&RepoScan>) -> Form {
    let strings = match language {
        Language::English => &ENGLISH,
        Language::Japanese => &JAPANESE,
    };
    let tabs = Category::ALL
        .iter()
        .map(|&category| {
            let title = title(category, language);
            let elements: Vec<Element> = category.elements().collect();
            let names: Vec<&str> = elements.iter().map(|element| element.name()).collect();
            Tab {
                title: title.to_owned(),
                prompt: strings
                    .prompt
                    .replace("{category}", title)
                    .replace("{elements}", &names.join(", ")),
                choices: strings
                    .reports
                    .iter()
                    .map(|&report| report.to_owned())
                    .collect(),
                checklist_on: 0,
                checklist_prompt: strings.checklist_prompt.to_owned(),
                items: names.iter().map(|&name| name.to_owned()).collect(),
                question: strings.question.to_owned(),
                material: names
                    .iter()
                    .map(|name| material(strings, name, scan))
                    .collect(),
            }
        })
        .collect();
    let [
        submit_title,
        guide,
        no_choice,
        blank_answer,
        ready,
        not_ready,
    ] = strings.text;
    Form {
        tabs,
        text: FormText {
            submit_title: submit_title.to_owned(),
            guide: guide.to_owned(),
            no_choice: no_choice.to_owned(),
            blank_answer: blank_answer.to_owned(),
            ready: ready.to_owned(),
            not_ready: not_ready.to_owned(),
        },
    }
}

/// The line on where `scan` found `element`, if it did.
fn material(strings: &Strings, element: &str, scan: Option<&RepoScan>) -> Vec<String> {
    let Some(places) = scan
        .and_then(|scan| scan.elements.get(element))
        .filter(|places| !places.is_empty())
    else {
        return Vec::new();
    };
    let places: Vec<String> = places
        .iter()
        .map(|place| format!("{}:{}", place.path, place.line))
        .collect();
    vec![
        strings
            .material
            .replace("{element}", element)
            .replace("{places}", &places.join(", ")),
    ]
}

/// What the user gave for one category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CategoryAnswer {
    pub category: Category,
    pub report: Report,
    /// The elements checked, when the report is "have written".
    pub checked: Vec<Element>,
    /// The question as the user saw it, with the checked elements' names.
    pub question: String,
    pub answer: String,
}

impl CategoryAnswer {
    /// Whether the answer is sent to be judged: "have written", an element checked, and an
    /// answer that is not blank (PRD UC1).
    pub(crate) fn sent(&self) -> bool {
        self.report == Report::HaveWritten
            && !self.checked.is_empty()
            && !self.answer.trim().is_empty()
    }
}

/// What the user gave for each category in `values`, submitted on `form`. Checks and an answer
/// kept under another report are not the user's for that category, and are left out.
pub(crate) fn read(form: &Form, values: &FormValues) -> Vec<CategoryAnswer> {
    Category::ALL
        .iter()
        .zip(&values.tabs)
        .enumerate()
        .map(|(tab, (&category, value))| {
            // A submitted form has a report on every tab.
            let report = value
                .choice
                .and_then(|choice| REPORTS.get(choice).copied())
                .unwrap_or(Report::DontKnow);
            let have_written = report == Report::HaveWritten;
            let elements: Vec<Element> = category.elements().collect();
            CategoryAnswer {
                category,
                report,
                checked: if have_written {
                    value
                        .checked
                        .iter()
                        .filter_map(|&item| elements.get(item).copied())
                        .collect()
                } else {
                    Vec::new()
                },
                question: form.question(tab, value),
                answer: if have_written {
                    value.answer.clone()
                } else {
                    String::new()
                },
            }
        })
        .collect()
}
