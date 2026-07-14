//! `ORCR_HOME` relocation + config-from-home integration tests (M0 acceptance:
//! "Config: `ORCR_HOME` relocation works"). No herdr required.

use orchestratr::config::Config;
use orchestratr::home::Home;
use orchestratr::store::Store;

#[test]
fn orcr_home_env_relocates_everything() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("relocated_home");

    // Honor the ORCR_HOME override (spec §14: relocates store, socket, lock, config,
    // logs, data all at once).
    std::env::set_var("ORCR_HOME", &root);
    let home = Home::ensure().expect("ensure relocated home");
    std::env::remove_var("ORCR_HOME");

    assert_eq!(home.root(), root);
    for p in [
        home.store_path(),
        home.socket_path(),
        home.lock_path(),
        home.config_path(),
        home.logs_dir(),
        home.data_dir(),
    ] {
        assert!(
            p.starts_with(&root),
            "{} should live under the relocated home",
            p.display()
        );
    }
    assert!(home.logs_dir().is_dir());
    assert!(home.data_dir().is_dir());

    // A store + config open under the relocated home.
    let store = Store::open(home.store_path()).expect("store under relocated home");
    assert_eq!(
        store.schema_version().unwrap(),
        orchestratr::store::SCHEMA_VERSION
    );
    assert!(home.store_path().exists());

    // Config defaults when no file present.
    let loaded = Config::load(&home).unwrap();
    assert_eq!(loaded.config.herdr.session, "orcr");
}

#[test]
fn config_loads_from_relocated_home_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    home.ensure_layout().unwrap();
    std::fs::write(
        home.config_path(),
        r#"{"defaults":{"agent":"codex"},"concurrency":{"max":7}}"#,
    )
    .unwrap();

    let loaded = Config::load(&home).unwrap();
    assert_eq!(loaded.config.defaults.agent, "codex");
    assert_eq!(loaded.config.concurrency.max, 7);
    assert!(loaded.warnings.is_empty());
}
