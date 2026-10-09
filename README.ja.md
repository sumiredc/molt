# molt

[English](README.md)

利用者が理解できる範囲の書き方でしかコードを書かない Rust コーディングエージェント。

依頼に利用者がまだ書いたことのない書き方が要るとき、molt は止まって解説し、読めないコードを黙って書く代わりに利用者に選ばせる。

## 前提

- [rustup](https://rustup.rs/)。toolchain の版は `rust-toolchain.toml` で固定していて、最初の `cargo` コマンドで rustup が入れる

## 始め方

```sh
cargo run -- --help
```

`cargo run -- --help` でサブコマンドの一覧が出ればビルドは動いている。この手順で確かめられるのは toolchain が揃っていることだけ。

## 次に読むもの

- 要件と設計：[PRD.ja.md](PRD.ja.md)
