pub mod path;

/// The user's approval of one operation that needs it: `cargo add` or a git commit (PRD §3, §8.4).
///
/// [`approve`] is the only way to create one, and the operation it approves takes it by value,
/// so an approval is used at most once.
///
/// It cannot be built outside `workspace`:
///
/// ```compile_fail
/// let approval = molt::workspace::Approval { _private: () };
/// ```
#[derive(Debug)]
pub struct Approval {
    _private: (),
}

/// Creates an [`Approval`]. Call it only after the user has approved the operation.
pub fn approve() -> Approval {
    Approval { _private: () }
}
