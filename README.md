# molt

[日本語版](README.ja.md)

A Rust coding agent that writes code only in the ways its user can understand.

When a request needs a construct the user has never written, molt pauses, explains it, and lets the user choose instead of silently writing code they cannot read.

## Prerequisites

- [rustup](https://rustup.rs/). The toolchain version is pinned in `rust-toolchain.toml`, and rustup installs it on the first `cargo` command

## Getting started

```sh
cargo run -- --help
```

If `cargo run -- --help` lists the subcommands, the build works. This confirms only that the toolchain is set up.

## Next reading

- Requirements and design: [PRD.md](PRD.md)
