//! `PocketDeck.exe --display <command>`: the service, its helpers and a few tools.
//!
//! | Command | Who runs it |
//! |---|---|
//! | `service` | the Service Control Manager |
//! | `arrange <device> <w> <h> <fps>`, `restore` | the service, in the signed-in session |
//! | `setup <driver zip>`, `remove` | PocketDeck, elevated |
//! | `status`, `on <w> <h> <fps>`, `keepalive`, `off` | anyone: asks the service over its pipe |
//! | `list` | anyone: prints the displays Windows sees |

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
        [command @ ("status" | "keepalive" | "off"), ..] | [command @ "on", ..] => {
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
            eprintln!("usage: PocketDeck.exe {FLAG} status | on <width> <height> <fps> | keepalive | off | list");
            setup::exit::USAGE
        }
    }
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
