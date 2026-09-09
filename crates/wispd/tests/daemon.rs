//! Daemon command configuration.

use wispd::config::NodeConfig;

#[test]
fn telemetry_configuration_uses_flag_then_environment_then_file() {
    let directory = tempfile::tempdir().expect("unique config directory");
    let mut cfg = NodeConfig::dev_default();
    cfg.otlp_logs_endpoint = Some("http://127.0.0.1:4318/from-file".parse().unwrap());
    let path = directory.path().join("wispd.toml");
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    for (environment, flag, expected) in [
        (None, None, "http://127.0.0.1:4318/from-file"),
        (
            Some("http://127.0.0.1:4318/from-env"),
            None,
            "http://127.0.0.1:4318/from-env",
        ),
        (
            Some("http://127.0.0.1:4318/from-env"),
            Some("http://127.0.0.1:4318/from-flag"),
            "http://127.0.0.1:4318/from-flag",
        ),
    ] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_wispd"));
        command
            .arg("--config")
            .arg(&path)
            .arg("--print-config")
            .env_remove("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT");
        if let Some(endpoint) = environment {
            command.env("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", endpoint);
        }
        if let Some(endpoint) = flag {
            command.arg("--otlp-logs-endpoint").arg(endpoint);
        }
        let output = command
            .output()
            .expect("the actual daemon configuration command must run");
        assert!(
            output.status.success(),
            "valid telemetry config must print: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let effective: NodeConfig =
            toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
        assert_eq!(
            String::from(effective.otlp_logs_endpoint.unwrap()),
            expected,
            "telemetry precedence must be flag, environment, then file"
        );
    }
}

#[test]
fn a_daemon_without_an_export_endpoint_keeps_export_disabled() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_wispd"))
        .arg("--print-config")
        .env_remove("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT")
        .output()
        .expect("the actual daemon configuration command must run");
    assert!(
        output.status.success(),
        "the existing default configuration must still print"
    );
    let effective: NodeConfig =
        toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    assert!(
        effective.otlp_logs_endpoint.is_none(),
        "the daemon must not silently select a telemetry destination"
    );
}
