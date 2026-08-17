use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_lists_list_and_run_commands() {
    Command::cargo_bin("hik-adapter")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("list").and(predicate::str::contains("run")));
}

#[test]
fn missing_config_is_nonzero_and_sanitized() {
    Command::cargo_bin("hik-adapter")
        .unwrap()
        .args(["list", "--config", "/definitely/missing/config.yaml"])
        .env("HIKCONNECT_PASSWORD", "password-sentinel")
        .assert()
        .failure()
        .stderr(predicate::str::contains("password-sentinel").not());
}

#[test]
fn missing_credentials_are_nonzero_and_sanitized() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.yaml");
    std::fs::write(&config, "recording:\n  duration_seconds: 30\n").unwrap();
    Command::cargo_bin("hik-adapter")
        .unwrap()
        .args(["list", "--config", config.to_str().unwrap()])
        .env_remove("HIKCONNECT_USERNAME")
        .env_remove("HIKCONNECT_PASSWORD")
        .assert()
        .failure()
        .stderr(predicate::str::contains("ConfigurationInvalid"));
}
