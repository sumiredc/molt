// expect: once_cell
use std::sync::OnceLock;

static CONFIG: OnceLock<String> = OnceLock::new();
