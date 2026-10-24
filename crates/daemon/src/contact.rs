//! Finding the daemon that serves this data directory, and starting it.
//!
//! `attach`, `open` and `stop` all meet the daemon the same way:
//!
//! 1. probe `GET /api/discovery` on 127.0.0.1 at the configured port;
//! 2. when nothing answers, start `companion-lobby run` from this very
//!    executable, windowless and detached, and wait for it to answer;
//! 3. before trusting what answers, verify that it serves this install (its
//!    fingerprint matches the attach secret in this data directory), that the
//!    daemon lock for this directory is held, that the lock holder's record
//!    names this port and the pid the document reports, and that the process
//!    listening on the port is that pid, so a process squatting on the port
//!    never receives the attach secret;
//! 4. when the verified daemon runs another build, ask it to stop, wait for
//!    it to let go of the lock, and start this build in its place.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::discovery::{self, DISCOVERY_PATH, Discovery, PRODUCT};
use crate::home::{self, display_path};
use crate::keyring::AttachSecret;
use crate::lock::{self, DaemonRecord};
use crate::loopback::{self, Call};

/// How long a discovery probe waits.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long each probe waits while a start is awaited.
const WAIT_PROBE_TIMEOUT: Duration = Duration::from_millis(300);
/// How often a start is polled.
const POLL: Duration = Duration::from_millis(50);
/// How long a stop may take before it is reported as failed.
pub const STOP_WAIT: Duration = Duration::from_secs(15);

/// A verified daemon.
#[derive(Debug, Clone)]
pub struct Daemon {
    /// The data directory it serves.
    pub dir: PathBuf,
    /// The port it listens on.
    pub port: u16,
    /// The install's attach secret, read only after verification.
    pub secret: AttachSecret,
    /// What it said about itself.
    pub discovery: Discovery,
}

/// What answers on the port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Nothing listens there.
    Nothing,
    /// Something that is not a Companion Lobby daemon.
    Foreign,
    /// A Companion Lobby daemon, not yet verified.
    Lobby(Discovery),
}

/// Asks the port what it is.
pub fn probe(port: u16) -> Probe {
    probe_within(port, PROBE_TIMEOUT)
}

/// Asks the port what it is, waiting at most `timeout` for each step.
pub fn probe_within(port: u16, timeout: Duration) -> Probe {
    match loopback::send(port, &Call::get(DISCOVERY_PATH, timeout)) {
        Err(_) => Probe::Nothing,
        Ok(answer) if answer.status == 200 => {
            match serde_json::from_slice::<Discovery>(&answer.body) {
                Ok(document) if document.product == PRODUCT && document.discovery_version == 1 => {
                    Probe::Lobby(document)
                }
                _ => Probe::Foreign,
            }
        }
        Ok(_) => Probe::Foreign,
    }
}

/// Finds, starts or restarts the daemon for `dir` on `port`. Everything,
/// a restart included, happens within `budget`, which is what a host waits
/// for its first answer.
pub fn ensure(dir: &Path, port: u16, budget: Duration) -> Result<Daemon, String> {
    let deadline = Instant::now() + budget;
    let own_build =
        discovery::build_id().map_err(|error| format!("cannot read this executable: {error}"))?;
    // No lock holder means no daemon serves this directory: start one at once
    // rather than wait for a probe of a closed port to time out.
    let running = lock::is_held(dir).unwrap_or(true);
    let document = match if running { probe(port) } else { Probe::Nothing } {
        Probe::Lobby(document) => document,
        Probe::Foreign => return Err(foreign(port)),
        Probe::Nothing => {
            refuse_other_port(dir, port)?;
            start(dir, port).map_err(|error| format!("cannot start the daemon: {error}"))?;
            wait_for(port, deadline)?
        }
    };
    let secret = verify(dir, port, &document)?;
    if document.build_id == own_build {
        return Ok(Daemon {
            dir: dir.to_path_buf(),
            port,
            secret,
            discovery: document,
        });
    }
    // A verified daemon of another build: replace it with this one, within
    // the same budget, because the host is waiting for its first answer.
    request_stop(port)?;
    if !wait_released(dir, deadline.saturating_duration_since(Instant::now())) {
        return Err(format!(
            "the daemon on 127.0.0.1:{port} runs another build and did not stop in time; start this MCP server again"
        ));
    }
    start(dir, port).map_err(|error| format!("cannot start the daemon: {error}"))?;
    let document = wait_for(port, deadline)?;
    let secret = verify(dir, port, &document)?;
    if document.build_id != own_build {
        return Err(format!(
            "the daemon on 127.0.0.1:{port} still runs another build after a restart"
        ));
    }
    Ok(Daemon {
        dir: dir.to_path_buf(),
        port,
        secret,
        discovery: document,
    })
}

fn foreign(port: u16) -> String {
    format!(
        "another program answers on 127.0.0.1:{port}; stop it, or give Companion Lobby another port with {}",
        crate::http::PORT_ENV
    )
}

/// A daemon for this directory that listens elsewhere would make a second start fail; say so.
fn refuse_other_port(dir: &Path, port: u16) -> Result<(), String> {
    match DaemonRecord::read(dir) {
        Ok(record) if record.port != port && lock::is_held(dir).unwrap_or(false) => Err(format!(
            "the daemon for {} listens on port {}, not {port}; use the same {} for both",
            display_path(dir),
            record.port,
            crate::http::PORT_ENV
        )),
        _ => Ok(()),
    }
}

fn wait_for(port: u16, deadline: Instant) -> Result<Discovery, String> {
    loop {
        match probe_within(port, WAIT_PROBE_TIMEOUT) {
            Probe::Lobby(document) => return Ok(document),
            Probe::Foreign => return Err(foreign(port)),
            Probe::Nothing if Instant::now() >= deadline => {
                return Err(format!(
                    "the daemon did not answer on 127.0.0.1:{port} in time; its trace.log in the data directory says why"
                ));
            }
            Probe::Nothing => std::thread::sleep(POLL),
        }
    }
}

/// Verifies that the daemon described by `document` serves `dir` and owns the
/// port, and only then reads the attach secret.
pub fn verify(dir: &Path, port: u16, document: &Discovery) -> Result<AttachSecret, String> {
    let other_install = || {
        format!(
            "the daemon on 127.0.0.1:{port} serves another data directory than {}",
            display_path(dir)
        )
    };
    let not_owner = |why: &str| {
        format!("refusing the process on 127.0.0.1:{port}: {why}; the attach secret was not sent")
    };
    let secret = AttachSecret::read(dir).map_err(|_| other_install())?;
    if secret.fingerprint() != document.store_fingerprint {
        return Err(other_install());
    }
    if !lock::is_held(dir)
        .map_err(|error| not_owner(&format!("cannot probe the daemon lock: {error}")))?
    {
        return Err(not_owner(
            "no process holds the daemon lock for this data directory",
        ));
    }
    let record =
        DaemonRecord::read(dir).map_err(|_| not_owner("the lock holder left no record"))?;
    let listeners = listener_pids(port).map_err(|error| not_owner(&error))?;
    if listeners.is_empty() || listeners.iter().any(|pid| *pid != record.pid) {
        return Err(not_owner("the listening process is not the lock holder"));
    }
    if record.port != port || record.pid != document.lock_holder_pid {
        return Err(not_owner(
            "it is not the process that holds the daemon lock",
        ));
    }
    Ok(secret)
}

/// The pids of every process listening on TCP `port`, on any local address.
pub fn listener_pids(port: u16) -> Result<Vec<u32>, String> {
    use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};
    let sockets = netstat2::get_sockets_info(
        AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
        ProtocolFlags::TCP,
    )
    .map_err(|error| format!("cannot read the TCP listener table: {error}"))?;
    let mut pids: Vec<u32> = sockets
        .into_iter()
        .filter(|socket| {
            matches!(&socket.protocol_socket_info, ProtocolSocketInfo::Tcp(tcp)
                if tcp.state == TcpState::Listen && tcp.local_port == port)
        })
        .flat_map(|socket| socket.associated_pids)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

/// Asks the daemon on `port` to drain and exit.
pub fn request_stop(port: u16) -> Result<(), String> {
    let origin = discovery::manager_origin(port);
    let call = Call {
        method: "POST",
        path: "/api/stop",
        headers: &[("Origin", &origin)],
        body: b"{}",
        timeout: PROBE_TIMEOUT,
    };
    match loopback::send(port, &call) {
        Ok(answer) if answer.status == 202 => Ok(()),
        Ok(answer) => Err(format!(
            "the daemon refused to stop (HTTP {})",
            answer.status
        )),
        Err(error) => Err(format!(
            "cannot reach the daemon on 127.0.0.1:{port}: {error}"
        )),
    }
}

/// Waits until no process holds the daemon lock for `dir`.
pub fn wait_released(dir: &Path, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if !lock::is_held(dir).unwrap_or(true) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Windows process creation flags for the daemon: no console window, no
/// console at all, its own process group so a host's Ctrl+C never reaches it.
#[cfg(windows)]
pub const DETACHED: u32 = 0x0800_0000 | 0x0000_0008 | 0x0000_0200;
/// Leaves the host's job object, so a host that kills its job on exit does not
/// take the daemon with it. Refused when the job forbids it.
#[cfg(windows)]
pub const BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Starts `companion-lobby run` for `dir` on `port`, detached from this process.
pub fn start(dir: &Path, port: u16) -> std::io::Result<()> {
    home::prepare(dir).map_err(|problem| std::io::Error::other(problem.to_string()))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("run")
        .current_dir(dir)
        .env(home::DATA_DIR_ENV, dir)
        .env(crate::http::PORT_ENV, port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The host's pipes are inheritable in this process; were the daemon to
        // inherit them, the host would never see this shim's stdout close.
        crate::win32::keep_std_handles_from_children();
        command.creation_flags(DETACHED | BREAKAWAY_FROM_JOB);
        match command.spawn() {
            Ok(_) => return Ok(()),
            // ERROR_ACCESS_DENIED: the job forbids breakaway; start inside it.
            Err(error) if error.raw_os_error() == Some(5) => {
                command.creation_flags(DETACHED);
            }
            Err(error) => return Err(error),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_port_answers_nothing() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        drop(listener);
        assert_eq!(probe(port), Probe::Nothing);
    }

    #[test]
    fn the_listener_table_names_this_process_for_its_own_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let pids = listener_pids(port).expect("table");
        assert_eq!(pids, vec![std::process::id()]);
    }

    #[cfg(windows)]
    #[test]
    fn the_daemon_starts_windowless_detached_and_in_its_own_group() {
        assert_eq!(DETACHED & 0x0800_0000, 0x0800_0000, "CREATE_NO_WINDOW");
        assert_eq!(DETACHED & 0x0000_0008, 0x0000_0008, "DETACHED_PROCESS");
        assert_eq!(
            DETACHED & 0x0000_0200,
            0x0000_0200,
            "CREATE_NEW_PROCESS_GROUP"
        );
        assert_eq!(DETACHED & BREAKAWAY_FROM_JOB, 0);
    }
}
