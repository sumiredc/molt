pub mod cargo;
pub mod git;
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
///
/// Nor can one approval be used twice:
///
/// ```compile_fail
/// use molt::workspace::approve;
/// use molt::workspace::cargo::{CargoRunner, CrateName, FakeCargo};
///
/// let fake = FakeCargo::new();
/// let name = CrateName::new("serde").unwrap();
/// let approval = approve();
/// let _ = fake.add(&name, approval);
/// let _ = fake.add(&name, approval);
/// ```
#[derive(Debug)]
pub struct Approval {
    _private: (),
}

/// Creates an [`Approval`]. Call it only after the user has approved the operation.
pub fn approve() -> Approval {
    Approval { _private: () }
}
