//! Daemon command configuration.

use isoplaned::config::NodeConfig;

#[test]
fn telemetry_configuration_uses_flag_then_environment_then_file() {
    for traces in [false, true] {
        let directory = tempfile::tempdir().expect("unique config directory");
        let mut cfg = NodeConfig::dev_default();
        let endpoint = Some("http://127.0.0.1:4318/from-file".parse().unwrap());
        let (environment_name, flag_name) = if traces {
            cfg.otlp_traces_endpoint = endpoint;
            (
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "--otlp-traces-endpoint",
            )
        } else {
            cfg.otlp_logs_endpoint = endpoint;
            ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", "--otlp-logs-endpoint")
        };
        let path = directory.path().join("isoplaned.toml");
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
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"));
            command
                .arg("--config")
                .arg(&path)
                .arg("--print-config")
                .env_remove("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT")
                .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
            if let Some(endpoint) = environment {
                command.env(environment_name, endpoint);
            }
            if let Some(endpoint) = flag {
                command.arg(flag_name).arg(endpoint);
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
                String::from(
                    if traces {
                        effective.otlp_traces_endpoint
                    } else {
                        effective.otlp_logs_endpoint
                    }
                    .unwrap()
                ),
                expected,
                "telemetry precedence must be flag, environment, then file"
            );
        }
    }
}

#[test]
fn a_daemon_without_an_export_endpoint_keeps_export_disabled() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"))
        .arg("--print-config")
        .env_remove("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .output()
        .expect("the actual daemon configuration command must run");
    assert!(
        output.status.success(),
        "the existing default configuration must still print"
    );
    let effective: NodeConfig =
        toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    assert!(
        effective.otlp_logs_endpoint.is_none() && effective.otlp_traces_endpoint.is_none(),
        "the daemon must not silently select a telemetry destination"
    );
}
