use scour_config::Config;

#[test]
fn older_configuration_receives_recovery_defaults() {
    let config: Config =
        toml::from_str("[service]\ncommit_idle_ms = 5000\n").expect("older config");
    assert_eq!(config.service.poll_interval_secs, 60);
    assert_eq!(config.service.reconcile_interval_secs, 1800);
}

#[test]
fn recovery_intervals_survive_configuration_round_trip() {
    let config: Config =
        toml::from_str("[service]\npoll_interval_secs = 120\nreconcile_interval_secs = 3600\n")
            .expect("config");
    let saved = toml::to_string(&config).expect("serialize");
    let restored: Config = toml::from_str(&saved).expect("restore");
    assert_eq!(restored.service.poll_interval_secs, 120);
    assert_eq!(restored.service.reconcile_interval_secs, 3600);
}
