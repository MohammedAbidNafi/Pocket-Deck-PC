//! `PocketDeck.exe --display <command>`: the service, its helpers and a few tools.
//!
//! | Command | Who runs it |
//! |---|---|
//! | `service` | the Service Control Manager |
//! | `arrange <device> <w> <h> <fps>`, `restore`, `follow <device>` | the service, in the signed-in session |
//! | `setup <driver zip>`, `remove` | PocketDeck, elevated |
//! | `status`, `on <w> <h> <fps>`, `keepalive`, `off` | anyone: asks the service over its pipe |
//! | `list` | anyone: prints the displays Windows sees |
//! | `remote status`, `remote on`, `remote off` | anyone: play from anywhere, via the service |
//! | `remote host [relay]`, `remote dial <pc id> [relay]` | testing: a tunnel in the foreground |

use crate::{FLAG, Mode, log, setup, topology};
use std::time::Duration;

/// Runs the display command when the program was started with [`FLAG`]. Returns the exit code,
/// or `None` for an ordinary start.
pub fn run_if_requested() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some(FLAG) {
        return None;
    }
    let rest: Vec<&str> = args[1..].iter().map(String::as_str).collect();
    if rest.first() != Some(&"service") {
        attach_console();
    }
    Some(run(&rest))
}

fn run(args: &[&str]) -> i32 {
    match args {
        ["service"] => match crate::service::run_as_service() {
            Ok(()) => 0,
            Err(e) => {
                log::line(format!("service dispatcher: {e}"));
                eprintln!("This command is for the Windows service only ({e}).");
                1
            }
        },
        ["arrange", device, w, h, fps] => match parse_mode(w, h, fps) {
            Some(mode) => match topology::arrange(device, mode, Duration::from_secs(15)) {
                Ok(name) => {
                    log::line(format!("arranged {name} at {mode}"));
                    0
                }
                Err(e) => {
                    log::line(format!("arrange: {e}"));
                    1
                }
            },
            None => setup::exit::USAGE,
        },
        ["follow", device] => crate::follow::run(device),
        ["restore"] => match topology::restore() {
            Ok(()) => 0,
            Err(e) => {
                log::line(format!("restore: {e}"));
                1
            }
        },
        ["setup", zip] => match setup::install(std::path::Path::new(zip)) {
            Ok(()) => setup::exit::OK,
            Err(f) => {
                log::line(format!("setup failed: {}", f.message));
                eprintln!("{}", f.message);
                f.code
            }
        },
        ["remove"] => match setup::remove() {
            Ok(()) => setup::exit::OK,
            Err(f) => {
                log::line(format!("remove failed: {}", f.message));
                eprintln!("{}", f.message);
                f.code
            }
        },
        ["ids"] => {
            for m in crate::sunshine_id::monitors() {
                println!("{} -> {}", m.instance_id, m.sunshine_id.as_deref().unwrap_or("(no stable ID)"));
            }
            0
        }
        ["list"] => {
            match crate::device::find() {
                Some(d) => println!("virtual display adapter: {} ({:?})", d.instance_id, d.state),
                None => println!("virtual display adapter: not installed"),
            }
            println!("settings: {}", crate::vdd::settings_file().display());
            match topology::describe() {
                Ok(lines) => lines.iter().for_each(|l| println!("{l}")),
                Err(e) => println!("{e}"),
            }
            0
        }
        ["remote", "host", relay @ ..] => remote_host(relay.first().copied()),
        ["remote", "dial", id, relay @ ..] => remote_dial(id, relay.first().copied()),
        [command @ ("status" | "keepalive" | "off" | "allow" | "disallow" | "remote"), ..] | [command @ "on", ..] => {
            match crate::pipe::request(&args.join(" ")) {
                Ok(reply) => {
                    println!("{reply}");
                    0
                }
                Err(e) => {
                    eprintln!("{command}: {e}");
                    1
                }
            }
        }
        _ => {
            eprintln!("usage: PocketDeck.exe {FLAG} status | on <width> <height> <fps> | keepalive | off | allow | disallow | list | ids");
            eprintln!("       PocketDeck.exe {FLAG} remote status | remote on | remote off | remote host [relay|none] | remote dial <pc id> [relay|none]");
            setup::exit::USAGE
        }
    }
}

/// The relay for the test tunnels: the one given, `none` for LAN only, else the configured one.
fn harness_relay(arg: Option<&str>) -> Option<String> {
    match arg {
        Some("none") => None,
        Some(url) => Some(url.to_string()),
        None => crate::tunnel::relay_url(),
    }
}

/// A key for the test tunnels, kept per user so the PC id stays the same between runs.
fn harness_secret(name: &str) -> [u8; 32] {
    let dir = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("PocketDeck");
    let path = dir.join(name);
    if let Some(key) = std::fs::read(&path).ok().and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()) {
        return key;
    }
    let key = pocketdeck_remote::generate_secret();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, key);
    key
}

/// `remote host`: forwards this PC's Sunshine to any phone or laptop that dials it, until
/// Ctrl+C. For testing only: it lets in every peer (Sunshine's own pairing still applies).
fn remote_host(relay: Option<&str>) -> i32 {
    let ports = match crate::tunnel::sunshine_ports() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let relay = harness_relay(relay);
    let config = pocketdeck_remote::Config::new(harness_secret("remote-harness-host.bin"), relay.clone());
    let host = match pocketdeck_remote::Host::start(config, std::sync::Arc::new(|_| true), ports) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    println!("PC id: {}", host.id());
    println!("relay: {}", relay.as_deref().unwrap_or("none (local network only)"));
    if relay.is_some() && !host.wait_online(Duration::from_secs(10)) {
        println!("warning: not connected to the relay yet");
    }
    println!("test only: every peer is let in. Dial with: PocketDeck.exe {FLAG} remote dial {}", host.id());
    let mut last = String::new();
    loop {
        let now = host.status();
        let line = format!("{} peer(s), {}", now.peers, now.to_line());
        if line != last {
            println!("{line}");
            last = line;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// `remote dial`: opens the tunnel to a PC and listens on 127.0.0.1 with Sunshine's ports, so
/// desktop Moonlight can add the PC as `127.0.0.1`. Runs until the tunnel closes.
fn remote_dial(id: &str, relay: Option<&str>) -> i32 {
    let pc = match pocketdeck_remote::parse_id(id) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("{e}");
            return setup::exit::USAGE;
        }
    };
    let config = pocketdeck_remote::Config::new(harness_secret("remote-harness-client.bin"), harness_relay(relay));
    let client = match pocketdeck_remote::Client::start(config, pc) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let p = client.ports();
    println!("tunnel up: add 127.0.0.1 in Moonlight (http {}, https {}, rtsp {}, udp {}/{}/{})", p.http, p.https, p.rtsp, p.video, p.control, p.audio);
    let mut last = String::new();
    while !client.is_closed() {
        let line = client.status().to_line();
        if line != last {
            println!("{line}");
            last = line;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("tunnel closed: {}", client.status().to_line());
    1
}

fn parse_mode(w: &str, h: &str, fps: &str) -> Option<Mode> {
    let mode = Mode { width: w.parse().ok()?, height: h.parse().ok()?, fps: fps.parse().ok()? };
    mode.sane().then_some(mode)
}

/// Lets output show up in the terminal that started a GUI-subsystem build.
fn attach_console() {
    // SAFETY: plain call; failing just means there's no console to write to.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_modes() {
        assert_eq!(parse_mode("2400", "1080", "120"), Some(Mode { width: 2400, height: 1080, fps: 120 }));
        assert_eq!(parse_mode("x", "1080", "120"), None);
        assert_eq!(parse_mode("1", "1", "1"), None);
    }

    #[test]
    fn unknown_commands_are_usage_errors() {
        assert_eq!(run(&["fly"]), setup::exit::USAGE);
        assert_eq!(run(&["arrange", "x", "1", "1", "1"]), setup::exit::USAGE);
    }
}
