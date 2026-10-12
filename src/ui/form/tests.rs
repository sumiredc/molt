use super::*;

/// A form of 2 tabs with choices `a`, `b`, `c`; `a` shows a checklist of `x`, `y`, `z`.
fn form() -> Form {
    let tab = |title: &str| Tab {
        title: title.to_owned(),
        prompt: format!("How about {title}?"),
        choices: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
        checklist_on: 0,
        checklist_prompt: "Which?".to_owned(),
        items: vec!["x".to_owned(), "y".to_owned(), "z".to_owned()],
        question: format!("Tell about {ITEMS}."),
        material: vec![vec!["x at src/a.rs:3".to_owned()]],
    };
    Form {
        tabs: vec![tab("one"), tab("two")],
        text: FormText {
            submit_title: "Submit".to_owned(),
            guide: "keys".to_owned(),
            no_choice: "No choice:".to_owned(),
            blank_answer: "Blank answer:".to_owned(),
            ready: "Enter submits.".to_owned(),
            not_ready: "Choose on every tab.".to_owned(),
        },
    }
}

fn press(state: &mut FormState<'_>, keys: &[Key]) -> Outcome {
    let mut outcome = Outcome::Continue;
    for &key in keys {
        outcome = state.press(key);
    }
    outcome
}

fn typed(text: &str) -> Vec<Key> {
    text.chars().map(Key::Char).collect()
}

/// Moves from choice `a` down to the answer: `a` → b → c → x, Space checks x, then y → z → answer.
fn to_answer(state: &mut FormState<'_>) {
    press(state, &[Key::Space]);
    press(state, &[Key::Down, Key::Down, Key::Down, Key::Space]);
    press(state, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(state.focus(), Focus::Answer);
}

#[test]
fn left_right_tab_and_back_tab_move_between_tabs_up_to_the_submit_tab() {
    let form = form();
    let mut state = FormState::new(&form, None);
    press(&mut state, &[Key::Right]);
    assert_eq!(state.tab(), 1);
    press(&mut state, &[Key::Right]);
    assert_eq!(
        state.tab(),
        2,
        "→ on the last tab of the form reaches the submit tab"
    );
    press(&mut state, &[Key::Right, Key::Tab]);
    assert_eq!(state.tab(), 2, "nothing past the submit tab");
    press(&mut state, &[Key::Left, Key::BackTab]);
    assert_eq!(state.tab(), 0);
    press(&mut state, &[Key::Left, Key::BackTab]);
    assert_eq!(state.tab(), 0, "nothing before the first tab");
    press(&mut state, &[Key::Tab]);
    assert_eq!(state.tab(), 1);
}

#[test]
fn in_the_answer_left_and_right_move_the_cursor_and_tab_still_moves_between_tabs() {
    let form = form();
    let mut state = FormState::new(&form, None);
    to_answer(&mut state);
    press(&mut state, &typed("abc"));
    press(&mut state, &[Key::Left, Key::Left]);
    assert_eq!((state.tab(), state.cursor()), (0, 1));
    press(&mut state, &[Key::Right]);
    assert_eq!((state.tab(), state.cursor()), (0, 2));
    press(&mut state, &[Key::Tab]);
    assert_eq!(state.tab(), 1);
    press(&mut state, &[Key::BackTab]);
    assert_eq!(state.tab(), 0);
}

#[test]
fn up_and_down_go_through_choices_checklist_and_answer_in_order() {
    let form = form();
    let mut state = FormState::new(&form, None);
    press(&mut state, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(
        state.focus(),
        Focus::Choice(2),
        "no checklist before the choice that shows it"
    );
    press(&mut state, &[Key::Up, Key::Up, Key::Space]);
    press(&mut state, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(state.focus(), Focus::Item(0));
    press(&mut state, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(
        state.focus(),
        Focus::Item(2),
        "no answer before an item is checked"
    );
    press(&mut state, &[Key::Space, Key::Down]);
    assert_eq!(state.focus(), Focus::Answer);
    press(&mut state, &[Key::Up, Key::Up, Key::Up, Key::Up]);
    assert_eq!(state.focus(), Focus::Choice(2));
}

#[test]
fn space_and_enter_choose_a_choice_and_toggle_an_item() {
    let form = form();
    let mut state = FormState::new(&form, None);
    press(&mut state, &[Key::Down, Key::Enter]);
    assert_eq!(state.values().tabs[0].choice, Some(1));
    press(
        &mut state,
        &[Key::Up, Key::Space, Key::Down, Key::Down, Key::Down],
    );
    press(&mut state, &[Key::Space, Key::Down, Key::Enter]);
    assert_eq!(state.values().tabs[0].checked, BTreeSet::from([0, 1]));
    press(&mut state, &[Key::Enter]);
    assert_eq!(state.values().tabs[0].checked, BTreeSet::from([0]));
}

#[test]
fn enter_in_the_answer_moves_to_the_next_tab_and_space_is_typed() {
    let form = form();
    let mut state = FormState::new(&form, None);
    to_answer(&mut state);
    press(&mut state, &typed("a"));
    press(&mut state, &[Key::Space]);
    press(&mut state, &typed("b"));
    assert_eq!(state.values().tabs[0].answer, "a b");
    press(&mut state, &[Key::Enter]);
    assert_eq!(state.tab(), 1);
}

#[test]
fn backspace_delete_home_and_end_edit_the_answer() {
    let form = form();
    let mut state = FormState::new(&form, None);
    to_answer(&mut state);
    press(&mut state, &typed("abcd"));
    press(&mut state, &[Key::Backspace, Key::Home, Key::Delete]);
    assert_eq!(state.values().tabs[0].answer, "bc");
    press(&mut state, &[Key::End]);
    press(&mut state, &typed("e"));
    assert_eq!(state.values().tabs[0].answer, "bce");
}

#[test]
fn characters_outside_the_answer_are_ignored() {
    let form = form();
    let mut state = FormState::new(&form, None);
    press(&mut state, &typed("q"));
    assert_eq!(state.values(), &form.empty_values());
}

#[test]
fn another_choice_or_no_checks_keep_the_checks_and_the_answer() {
    let form = form();
    let mut state = FormState::new(&form, None);
    to_answer(&mut state);
    press(&mut state, &typed("kept"));
    let entered = state.values().tabs[0].clone();

    // Choosing `c` hides the checklist; choosing `a` again shows what was entered.
    press(
        &mut state,
        &[Key::Up, Key::Up, Key::Up, Key::Up, Key::Space],
    );
    assert_eq!(state.focus(), Focus::Choice(2));
    let hidden = &state.values().tabs[0];
    assert_eq!(
        (hidden.choice, &hidden.checked, hidden.answer.as_str()),
        (Some(2), &entered.checked, "kept")
    );
    press(&mut state, &[Key::Up, Key::Up, Key::Space]);
    assert_eq!(state.values().tabs[0], entered);

    // Unchecking every item hides the answer and keeps it.
    press(&mut state, &[Key::Down, Key::Down, Key::Down, Key::Space]);
    assert!(state.values().tabs[0].checked.is_empty());
    assert_eq!(state.values().tabs[0].answer, "kept");
    press(&mut state, &[Key::Space]);
    assert_eq!(state.values().tabs[0], entered);
}

#[test]
fn the_submit_tab_lists_tabs_without_a_choice_and_with_a_blank_answer() {
    let form = form();
    let mut state = FormState::new(&form, None);
    to_answer(&mut state);
    press(&mut state, &typed("  "));
    press(&mut state, &[Key::Tab, Key::Tab]);
    let screen = render(&state, 80);
    let at = |text: &str| screen.lines.iter().position(|line| line == text);
    let no_choice = at("No choice:").unwrap();
    assert_eq!(screen.lines[no_choice + 1], "  - two");
    let blank = at("Blank answer:").unwrap();
    assert_eq!(screen.lines[blank + 1], "  - one");
    assert!(at("Choose on every tab.").is_some());
}

#[test]
fn enter_on_the_submit_tab_submits_only_when_every_tab_has_a_choice() {
    let form = form();
    let mut state = FormState::new(&form, None);
    press(&mut state, &[Key::Enter, Key::Tab, Key::Tab]);
    assert_eq!(press(&mut state, &[Key::Enter]), Outcome::Continue);

    press(&mut state, &[Key::BackTab, Key::Down, Key::Enter, Key::Tab]);
    assert_eq!(press(&mut state, &[Key::Enter]), Outcome::Submit);
    let screen = render(&state, 80);
    assert!(screen.lines.iter().any(|line| line == "Enter submits."));
    assert!(!screen.lines.iter().any(|line| line == "No choice:"));
    assert_eq!(state.values().tabs[0].choice, Some(0));
    assert_eq!(state.values().tabs[1].choice, Some(1));
}

#[test]
fn ctrl_c_or_ctrl_d_interrupts_anywhere() {
    let form = form();
    let mut state = FormState::new(&form, None);
    assert_eq!(state.press(Key::Interrupt), Outcome::Interrupt);
    to_answer(&mut state);
    assert_eq!(state.press(Key::Interrupt), Outcome::Interrupt);
    press(&mut state, &[Key::Tab, Key::Tab]);
    assert_eq!(state.press(Key::Interrupt), Outcome::Interrupt);
}

#[test]
fn the_form_opens_with_the_initial_values() {
    let form = form();
    let initial = FormValues {
        tabs: vec![
            TabValues {
                choice: Some(2),
                checked: BTreeSet::from([1]),
                answer: "earlier".to_owned(),
            },
            TabValues {
                choice: Some(1),
                ..TabValues::default()
            },
        ],
    };
    let mut state = FormState::new(&form, Some(&initial));
    assert_eq!(state.values(), &initial);
    assert_eq!(
        state.focus(),
        Focus::Choice(2),
        "the focus is on the chosen choice"
    );
    press(&mut state, &[Key::Up, Key::Up, Key::Space]);
    press(&mut state, &[Key::Down; 6]);
    assert_eq!(state.focus(), Focus::Answer);
    assert_eq!(
        state.cursor(),
        "earlier".len(),
        "the cursor is at the end of the answer"
    );
}

#[test]
fn initial_values_for_tabs_the_form_does_not_have_are_ignored() {
    let form = form();
    let initial = FormValues {
        tabs: vec![TabValues {
            choice: Some(1),
            ..TabValues::default()
        }],
    };
    let state = FormState::new(&form, Some(&initial));
    assert_eq!(state.values().tabs.len(), 2);
    assert_eq!(state.values().tabs[0].choice, Some(1));
    assert_eq!(state.values().tabs[1], TabValues::default());
}

#[test]
fn a_japanese_answer_moves_the_cursor_one_char_at_a_time() {
    let form = form();
    let mut state = FormState::new(&form, None);
    to_answer(&mut state);
    press(&mut state, &typed("日本語"));
    assert_eq!(state.cursor(), 3);
    press(&mut state, &[Key::Left, Key::Backspace]);
    assert_eq!(state.values().tabs[0].answer, "日語");
    press(&mut state, &typed("本"));
    assert_eq!(state.values().tabs[0].answer, "日本語");
    press(&mut state, &[Key::Delete]);
    assert_eq!(state.values().tabs[0].answer, "日本");
}

#[test]
fn the_cursor_of_a_japanese_answer_is_shown_at_its_full_width() {
    let form = form();
    let mut state = FormState::new(&form, None);
    to_answer(&mut state);
    press(&mut state, &typed("日本語"));
    press(&mut state, &[Key::Left]);
    let screen = render(&state, 80);
    let (row, column) = screen.cursor.unwrap();
    assert_eq!(screen.lines[row], "> 日本語");
    assert_eq!(
        column,
        2 + 4,
        "after the pointer, 2 columns for each of 日 and 本"
    );
}

#[test]
fn wrapping_counts_full_width_chars_as_2_columns() {
    assert_eq!(wrap("あいう", 4), ["あい", "う"]);
    assert_eq!(wrap("abあ", 3), ["ab", "あ"]);
    assert_eq!(wrap("", 4), [""]);
}

#[test]
fn the_cursor_follows_the_wrapped_answer() {
    assert_eq!(wrap_with_cursor("あいう", 2, 4).1, (1, 0));
    assert_eq!(wrap_with_cursor("あいう", 3, 4).1, (1, 2));
    assert_eq!(
        wrap_with_cursor("あい", 2, 4).1,
        (1, 0),
        "past a full last line, at the start of the next row"
    );
    assert_eq!(wrap_with_cursor("", 0, 4).1, (0, 0));
}

#[test]
fn the_question_names_the_checked_items_with_the_material_under_it() {
    let form = form();
    let mut state = FormState::new(&form, None);
    press(
        &mut state,
        &[Key::Space, Key::Down, Key::Down, Key::Down, Key::Space],
    );
    press(&mut state, &[Key::Down, Key::Down, Key::Space]);
    let screen = render(&state, 80);
    let question = screen
        .lines
        .iter()
        .position(|line| line == "Tell about x, z.")
        .unwrap();
    assert_eq!(screen.lines[question + 1], "  x at src/a.rs:3");
}

#[test]
fn the_row_of_tabs_brackets_the_open_tab_and_wraps() {
    let form = form();
    let mut state = FormState::new(&form, None);
    press(&mut state, &[Key::Tab]);
    assert_eq!(render(&state, 80).lines[0], " one  [two]  Submit ");
    let narrow = render(&state, 12);
    assert_eq!(&narrow.lines[..3], [" one  [two]", " Submit ", ""]);
}

#[test]
fn every_tab_ends_with_the_key_guide() {
    let form = form();
    let mut state = FormState::new(&form, None);
    assert_eq!(render(&state, 80).lines.last().unwrap(), "keys");
    press(&mut state, &[Key::Tab, Key::Tab]);
    assert_eq!(render(&state, 80).lines.last().unwrap(), "keys");
}
