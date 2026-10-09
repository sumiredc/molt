//! `molt doctor` を実行ファイルとして起動して確かめる。ネットワークには出ない。

use assert_cmd::Command;

#[test]
fn doctor_with_fake_stops_without_connecting_or_writing_config() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.toml");
    // 届けば確かめが始まってしまう接続先。--fake で止まるなら、ここには送らない
    std::fs::write(
        &config,
        "[api]\nbase_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"\"\n",
    )
    .unwrap();
    let before = std::fs::read_to_string(&config).unwrap();

    let output = Command::cargo_bin("molt")
        .unwrap()
        .env("MOLT_HOME", home.path())
        .args(["--fake", "script.toml", "doctor"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("doctor does not support --fake yet"),
        "{stderr}"
    );
    assert!(!stderr.contains("connection failed"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}
