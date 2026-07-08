use std::fs;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use orchestratr::mock::{extract_response_path, parse_directives};

const QUIET_WINDOW: Duration = Duration::from_millis(200);
const SLEEP_DRAIN_INTERVAL: Duration = Duration::from_millis(50);

fn main() -> io::Result<()> {
    let rx = spawn_stdin_reader();
    let mut stdout = io::stdout();
    writeln!(stdout, "MOCK_READY")?;
    stdout.flush()?;

    let mut turn = 1_u64;
    loop {
        let Ok(line) = rx.recv() else {
            return Ok(());
        };
        if line.trim().is_empty() {
            continue;
        }
        if parse_directives(&line).exit {
            return Ok(());
        }

        writeln!(stdout, "MOCK_WORKING")?;
        stdout.flush()?;

        let mut prompt = line;
        collect_until_quiet(&rx, &mut prompt)?;

        let mut directives = parse_directives(&prompt);
        if directives.exit {
            return Ok(());
        }
        if directives.block {
            writeln!(stdout, "MOCK_BLOCKED")?;
            stdout.flush()?;
            match rx.recv() {
                Ok(line) => {
                    prompt.push('\n');
                    prompt.push_str(&line);
                    collect_until_quiet(&rx, &mut prompt)?;
                }
                Err(_) => return Ok(()),
            }
            directives = parse_directives(&prompt);
            if directives.exit {
                return Ok(());
            }
        }

        if let Some(ms) = directives.sleep_ms {
            drain_during_sleep(&rx, &mut prompt, Duration::from_millis(ms))?;
            directives = parse_directives(&prompt);
            if directives.exit {
                return Ok(());
            }
        }

        if !directives.ignore_out {
            if let Some(path) = extract_response_path(&prompt) {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, format!("# mock response\n{prompt}"))?;
            }
        }

        writeln!(stdout, "MOCK_DONE {turn}")?;
        stdout.flush()?;
        turn += 1;
    }
}

fn spawn_stdin_reader() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn collect_until_quiet(rx: &Receiver<String>, prompt: &mut String) -> io::Result<()> {
    loop {
        match rx.recv_timeout(QUIET_WINDOW) {
            Ok(line) => {
                prompt.push('\n');
                prompt.push_str(&line);
            }
            Err(RecvTimeoutError::Timeout) => return Ok(()),
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn drain_during_sleep(
    rx: &Receiver<String>,
    prompt: &mut String,
    duration: Duration,
) -> io::Result<()> {
    let start = Instant::now();
    while start.elapsed() < duration {
        let remaining = duration.saturating_sub(start.elapsed());
        let wait = remaining.min(SLEEP_DRAIN_INTERVAL);
        match rx.recv_timeout(wait) {
            Ok(line) => {
                prompt.push('\n');
                prompt.push_str(&line);
                collect_until_quiet(rx, prompt)?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
    Ok(())
}
