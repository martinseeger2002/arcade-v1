//! arcade-signer --home DIR
//!
//! Holds the wallet seed for one arcade data folder and signs only the transaction the desktop
//! window approved. Started by arcaded; exits with it on Linux.

fn main() {
    die_with_parent();
    let mut args = std::env::args().skip(1);
    let mut home = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--home" => home = args.next().map(std::path::PathBuf::from),
            "--help" | "-h" => {
                println!("arcade-signer --home DIR\n\nHolds this arcade's wallet seed and signs on signer/signer.sock. Not a network service.");
                return;
            }
            other => {
                eprintln!("arcade-signer: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let home = home.unwrap_or_else(arcade_local::default_home);
    if let Err(e) = arcade_signer::serve(&home) {
        eprintln!("arcade-signer: {e}");
        std::process::exit(1);
    }
}

/// On Linux, stop when arcaded (the parent) stops, including a crash. Otherwise a killed
/// service would leave the unlocked-or-not process sitting on the socket.
#[cfg(target_os = "linux")]
fn die_with_parent() {
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
        if libc::getppid() == 1 {
            libc::raise(libc::SIGTERM);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn die_with_parent() {}
