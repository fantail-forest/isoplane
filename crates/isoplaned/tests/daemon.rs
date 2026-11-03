use isoplaned::config::NodeConfig;

#[test]
fn telemetry_configuration_uses_flag_then_environment_then_file() {
    for traces in [false, true] {
        let directory = tempfile::tempdir().expect("unique config directory");
        let mut cfg = NodeConfig::new("operator");
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
                .arg("serve")
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
            let output = isoplane_test_support::run_successfully(
                &mut command,
                "print a valid telemetry config",
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
fn checking_configuration_never_creates_or_retires_installation_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.toml");
    std::fs::write(&path, "owner = 'operator'\ndata_dir = 'state'\n").unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"));
    command.args(["serve", "--check", "--config"]).arg(&path);
    isoplane_test_support::run_successfully(&mut command, "static configuration check");
    assert!(!directory.path().join("state").exists());
    let state = directory.path().join("state");
    std::fs::create_dir_all(state.join("selected")).unwrap();
    let existing = state.join("selected/invalid.json");
    std::fs::write(&existing, b"existing-selection").unwrap();
    isoplane_test_support::run_successfully(&mut command, "read-only static configuration check");
    assert_eq!(std::fs::read(existing).unwrap(), b"existing-selection");
}
