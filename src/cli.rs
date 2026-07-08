use std::io::{self, IsTerminal};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::herdr::{discover_herdr, HerdrClient, HerdrError, INSTALL_URL};
use crate::store::Store;

#[derive(Debug, Parser)]
#[command(name = "orcr", version, about = "Agent orchestration over herdr")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Serialize)]
struct StatusReport {
    herdr: HerdrStatus,
    session: SessionStatus,
    store: StoreStatus,
    db: DbStatus,
}

#[derive(Debug, Serialize)]
struct HerdrStatus {
    path: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct SessionStatus {
    name: String,
    exists: bool,
    running: bool,
    session_dir: Option<String>,
    socket_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct StoreStatus {
    root: String,
    ok: bool,
}

#[derive(Debug, Serialize)]
struct DbStatus {
    path: String,
    ok: bool,
    error: Option<String>,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Status { json: false }) {
        Command::Status { json } => run_status(json),
    }
}

fn run_status(json_output: bool) -> ExitCode {
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => return print_error(json_output, "config_error", &error.to_string(), 2),
    };

    let herdr_bin = match discover_herdr(&config.herdr.bin) {
        Ok(path) => path,
        Err(HerdrError::NotFound) => {
            let message = format!("herdr was not found; install it from {INSTALL_URL}");
            return print_error(json_output, "herdr_missing", &message, 2);
        }
        Err(error) => return print_error(json_output, "herdr_error", &error.to_string(), 2),
    };

    let client = HerdrClient::new(herdr_bin.clone(), config.herdr.session.clone());
    let version = match client.version() {
        Ok(version) => version,
        Err(error) => return print_error(json_output, "herdr_error", &error.to_string(), 2),
    };

    let session = match client.session_list() {
        Ok(list) => list
            .sessions
            .into_iter()
            .find(|session| session.name == config.herdr.session)
            .map(|session| SessionStatus {
                name: session.name,
                exists: true,
                running: session.running,
                session_dir: session.session_dir,
                socket_path: session.socket_path,
            })
            .unwrap_or_else(|| SessionStatus {
                name: config.herdr.session.clone(),
                exists: false,
                running: false,
                session_dir: None,
                socket_path: None,
            }),
        Err(error) => return print_error(json_output, "herdr_error", &error.to_string(), 2),
    };

    let db_path = config.store_root.join("orcr.db");
    let db_error = Store::open(&config.store_root)
        .err()
        .map(|error| error.to_string());
    let report = StatusReport {
        herdr: HerdrStatus {
            path: herdr_bin.display().to_string(),
            version,
        },
        session,
        store: StoreStatus {
            root: config.store_root.display().to_string(),
            ok: true,
        },
        db: DbStatus {
            path: db_path.display().to_string(),
            ok: db_error.is_none(),
            error: db_error,
        },
    };

    if json_output || !io::stdout().is_terminal() {
        println!("{}", json!({"ok": true, "result": report}));
    } else {
        print_human_status(&report);
    }
    ExitCode::SUCCESS
}

fn print_error(json_output: bool, code: &str, message: &str, exit_code: u8) -> ExitCode {
    if json_output {
        println!(
            "{}",
            json!({"ok": false, "error": {"code": code, "message": message}})
        );
    } else {
        eprintln!("{message}");
    }
    ExitCode::from(exit_code)
}

fn print_human_status(report: &StatusReport) {
    println!("herdr: {} ({})", report.herdr.version, report.herdr.path);
    println!(
        "session: {} ({})",
        report.session.name,
        if report.session.running {
            "running"
        } else if report.session.exists {
            "stopped"
        } else {
            "missing"
        }
    );
    println!("store: {} (ok)", report.store.root);
    if report.db.ok {
        println!("db: {} (ok)", report.db.path);
    } else {
        println!(
            "db: {} (error: {})",
            report.db.path,
            report.db.error.as_deref().unwrap_or("unknown")
        );
    }
}
