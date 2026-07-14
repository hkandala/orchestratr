//! Handshake rejection test (M0 acceptance: "version handshake rejects a fabricated
//! protocol number"). This runs without herdr by pointing the driver at a stub Unix
//! socket that returns a `pong` with a bad protocol number.

use orchestratr::driver::HerdrDriver;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::thread;

/// Spawn a one-shot stub herdr socket that replies to a single request with the given
/// pong protocol number, then closes (mirroring herdr's one-request-per-connection).
fn spawn_stub(path: std::path::PathBuf, protocol: u32) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(&path).expect("bind stub socket");
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Read the one request line.
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let req: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
            let id = req.get("id").and_then(|v| v.as_str()).unwrap_or("x");
            let resp = serde_json::json!({
                "id": id,
                "result": { "type": "pong", "version": "9.9.9", "protocol": protocol }
            });
            let mut out = serde_json::to_vec(&resp).unwrap();
            out.push(b'\n');
            let _ = stream.write_all(&out);
            let _ = stream.flush();
        }
    })
}

#[test]
fn rejects_fabricated_low_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("stub.sock");
    let h = spawn_stub(sock.clone(), 1); // fabricated, below the required minimum (16)

    let err = match HerdrDriver::connect(&sock) {
        Ok(_) => panic!("driver accepted a herdr reporting protocol 1"),
        Err(e) => e,
    };
    assert_eq!(err.details["cause"], "unsupported_version");
    assert_eq!(err.exit_code(), 2);
    h.join().unwrap();
}

#[test]
fn accepts_matching_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("stub_ok.sock");
    let h = spawn_stub(sock.clone(), 16); // the required minimum

    let driver = HerdrDriver::connect(&sock).expect("driver should accept protocol 16");
    assert_eq!(driver.protocol(), 16);
    h.join().unwrap();
}
