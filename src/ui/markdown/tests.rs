use super::*;

fn render(lines: &[&str]) -> Vec<Styled> {
    let mut renderer = Renderer::default();
    lines.iter().map(|line| renderer.line(line)).collect()
}

#[test]
fn a_heading_loses_its_hashes() {
    assert_eq!(
        render(&["## expect（エラー処理）", "#nothashtag"]),
        [
            Styled::Heading("expect（エラー処理）".to_owned()),
            Styled::Prose(vec![Span::Plain("#nothashtag".to_owned())]),
        ]
    );
}

#[test]
fn emphasis_and_inline_code_are_spans() {
    assert_eq!(
        render(&["use `expect` for **now**, _not_ later *ok"]),
        [Styled::Prose(vec![
            Span::Plain("use ".to_owned()),
            Span::Code("expect".to_owned()),
            Span::Plain(" for ".to_owned()),
            Span::Bold("now".to_owned()),
            Span::Plain(", ".to_owned()),
            Span::Italic("not".to_owned()),
            Span::Plain(" later *ok".to_owned()),
        ])]
    );
}

#[test]
fn a_code_block_is_known_by_its_fences_and_markdown_inside_it_is_left_alone() {
    assert_eq!(
        render(&["```rust", "## not a heading", "```", "## a heading"]),
        [
            Styled::Code(None),
            Styled::Code(Some("## not a heading".to_owned())),
            Styled::Code(None),
            Styled::Heading("a heading".to_owned()),
        ]
    );
}

#[test]
fn a_list_item_keeps_a_bullet() {
    assert_eq!(
        render(&["- one", "  * two"]),
        [
            Styled::Prose(vec![
                Span::Plain("• ".to_owned()),
                Span::Plain("one".to_owned())
            ]),
            Styled::Prose(vec![
                Span::Plain("  • ".to_owned()),
                Span::Plain("two".to_owned())
            ]),
        ]
    );
}

#[test]
fn code_blocks_are_folded_with_their_line_count_and_inline_code_is_not() {
    let text = "I will write:\n```rust\nlet a = Arc::new(1);\nlet b = 2;\n```\nThen `Arc<Mutex<T>>`.\n```\none\n```";

    assert_eq!(
        fold(text),
        "I will write:\n[code: 2 lines]\nThen `Arc<Mutex<T>>`.\n[code: 1 line]"
    );
}

#[test]
fn a_code_block_not_closed_is_folded_to_the_end() {
    assert_eq!(fold("start\n```\na\nb\nc"), "start\n[code: 3 lines]");
}

#[test]
fn only_the_first_code_block_of_an_explanation_is_held_back() {
    let mut hold = HoldFirstCode::default();
    let lines = [
        "idea",
        "```rust",
        "fn main() {}",
        "```",
        "after",
        "```",
        "second",
        "```",
    ];

    let passed: Vec<&str> = lines.into_iter().filter(|line| hold.passes(line)).collect();

    assert_eq!(passed, ["idea", "after", "```", "second", "```"]);
}
