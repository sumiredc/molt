//! The [`CargoRunner`] for tests: returns queued results and records the calls.

use std::cell::RefCell;
use std::collections::VecDeque;

use super::{CargoError, CargoOutput, CargoRunner, CrateName, Subcommand};
use crate::workspace::Approval;

/// One call made to [`FakeCargo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoCall {
    Run(Subcommand),
    Add(CrateName),
}

/// A [`CargoRunner`] that starts no process. Each call, `run` or `add`, takes the next queued
/// result; when none is left it returns [`CargoError::FakeExhausted`].
#[derive(Debug, Default)]
pub struct FakeCargo {
    results: RefCell<VecDeque<Result<CargoOutput, CargoError>>>,
    calls: RefCell<Vec<CargoCall>>,
}

impl FakeCargo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the result of the next call.
    pub fn push(&self, result: Result<CargoOutput, CargoError>) {
        self.results.borrow_mut().push_back(result);
    }

    /// The calls made so far, in order.
    pub fn calls(&self) -> Vec<CargoCall> {
        self.calls.borrow().clone()
    }

    fn next(&self, call: CargoCall) -> Result<CargoOutput, CargoError> {
        self.calls.borrow_mut().push(call);
        self.results
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(CargoError::FakeExhausted))
    }
}

impl CargoRunner for FakeCargo {
    fn run(&self, subcommand: Subcommand) -> Result<CargoOutput, CargoError> {
        self.next(CargoCall::Run(subcommand))
    }

    fn add(&self, crate_name: &CrateName, _approval: Approval) -> Result<CargoOutput, CargoError> {
        self.next(CargoCall::Add(crate_name.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::approve;

    fn output(stdout: &str) -> CargoOutput {
        CargoOutput {
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    fn serde() -> CrateName {
        CrateName::new("serde").unwrap()
    }

    #[test]
    fn queued_results_are_returned_in_order_across_run_and_add() {
        let fake = FakeCargo::new();
        fake.push(Ok(output("first")));
        fake.push(Ok(output("second")));

        assert_eq!(fake.run(Subcommand::Check).unwrap(), output("first"));
        assert_eq!(fake.add(&serde(), approve()).unwrap(), output("second"));
    }

    #[test]
    fn a_queued_error_is_returned_in_its_turn() {
        let fake = FakeCargo::new();
        fake.push(Err(CargoError::FakeExhausted));
        fake.push(Ok(output("after the error")));

        assert!(matches!(
            fake.run(Subcommand::Test),
            Err(CargoError::FakeExhausted)
        ));
        assert_eq!(
            fake.run(Subcommand::Test).unwrap(),
            output("after the error")
        );
    }

    #[test]
    fn calls_are_recorded_in_order() {
        let fake = FakeCargo::new();
        fake.push(Ok(output("")));
        fake.push(Ok(output("")));
        fake.push(Ok(output("")));

        fake.run(Subcommand::Clippy).unwrap();
        fake.add(&serde(), approve()).unwrap();
        fake.run(Subcommand::Check).unwrap();

        assert_eq!(
            fake.calls(),
            [
                CargoCall::Run(Subcommand::Clippy),
                CargoCall::Add(serde()),
                CargoCall::Run(Subcommand::Check),
            ]
        );
    }

    #[test]
    fn a_call_after_the_queue_runs_out_is_exhausted_and_still_recorded() {
        let fake = FakeCargo::new();

        assert!(matches!(
            fake.run(Subcommand::Build),
            Err(CargoError::FakeExhausted)
        ));
        assert_eq!(fake.calls(), [CargoCall::Run(Subcommand::Build)]);
    }
}
