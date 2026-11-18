//! **The guard**: `cauldron-inference --guard -- <program> <args…>`, a small
//! process between the service and its `llama-server` child, so the child
//! does not outlive the service, however the service ends, unless the guard
//! is killed with it (then the service's next start stops the child, from
//! `child.json`).
//!
//! The guard starts the program (its output to the guard's standard error,
//! which the service points at the child's log), prints `pid <n>` on its
//! standard output, and then:
//!
//! - when its standard input ends (the service closed it to unload, or the
//!   service is gone, killed or not: the kernel closes the pipe either way),
//!   or when it is sent TERM, INT or HUP, it sends the program TERM, waits up
//!   to [`GRACE`] for it to exit, and then kills it;
//! - when the program exits, the guard exits with its status (the exit code,
//!   or 128 + the signal), which is how the service learns of a crash.
//!
//! Signals are sent with `/bin/kill`, so no unsafe code is needed.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long the program has to exit after TERM before it is killed.
pub const GRACE: Duration = Duration::from_secs(10);

/// Send `sig` (a name: `TERM`, `KILL`) to `pid`.
pub fn signal(pid: u32, sig: &str) -> bool {
    Command::new("/bin/kill").arg(format!("-{sig}")).arg(pid.to_string()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Is a process `pid` alive (and not a zombie of ours)?
pub fn alive(pid: u32) -> bool {
    Command::new("/bin/kill").arg("-0").arg(pid.to_string()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Run the guard over `argv` (`[program, args…]`); the exit code.
pub fn run(argv: &[std::ffi::OsString]) -> i32 {
    let Some((prog, args)) = argv.split_first() else {
        eprintln!("cauldron-inference --guard: no program given");
        return 2;
    };
    let stderr = std::io::stderr();
    let out = match stderr.as_fd().try_clone_to_owned() {
        Ok(fd) => Stdio::from(fd),
        Err(_) => Stdio::null(),
    };
    let mut child = match Command::new(prog).args(args).stdin(Stdio::null()).stdout(out).stderr(Stdio::inherit()).spawn() {
        Ok(c) => c,
        Err(e) => {
            println!("error {} could not be started: {e}", std::path::Path::new(prog).display());
            return 127;
        }
    };
    let pid = child.id();
    {
        let mut so = std::io::stdout();
        let _ = writeln!(so, "pid {pid}");
        let _ = so.flush();
    }
    let exited = Arc::new(AtomicBool::new(false));
    let stop = {
        let exited = exited.clone();
        move || {
            if exited.load(Ordering::SeqCst) {
                return;
            }
            signal(pid, "TERM");
            let deadline = Instant::now() + GRACE;
            while Instant::now() < deadline {
                if exited.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if !exited.load(Ordering::SeqCst) {
                signal(pid, "KILL");
            }
        }
    };
    // The service's end of the pipe: closed means stop.
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut sink = [0u8; 256];
            let mut stdin = std::io::stdin();
            loop {
                match stdin.read(&mut sink) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            stop();
        });
    }
    // A signal to the guard itself.
    if let Ok(mut signals) = signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT, signal_hook::consts::SIGHUP]) {
        let stop = stop.clone();
        std::thread::spawn(move || {
            if signals.forever().next().is_some() {
                stop();
            }
        });
    }
    let status = child.wait();
    exited.store(true, Ordering::SeqCst);
    match status {
        Ok(s) => {
            use std::os::unix::process::ExitStatusExt;
            s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
        }
        Err(_) => 1,
    }
}
