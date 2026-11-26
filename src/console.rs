//! The console: `docker` commands in pseudo-terminals (shells in containers,
//! logs, builds, Compose), their screens parsed by `vt100`.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

const SCROLLBACK: usize = 10_000;

/// Counts what the commands print (and their ends): the repaint waits for it.
pub static OUTPUT: AtomicU64 = AtomicU64::new(0);

fn nonzero(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap_or(NonZeroU16::MIN)
}

pub struct Session {
    parser: Mutex<vt100::Parser>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    /// The code it ended with.
    code: Mutex<Option<u32>>,
    size: Mutex<(u16, u16)>,
}

impl Session {
    /// Runs `program` with `args`; `on_end` hears of its end.
    pub fn spawn(
        program: &str,
        args: &[String],
        dir: Option<&std::path::Path>,
        on_end: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Session>, String> {
        let mut command = CommandBuilder::new(program);
        command.args(args);
        if let Some(dir) = dir.filter(|d| d.is_dir()) {
            command.cwd(dir);
        } else if let Ok(here) = std::env::current_dir() {
            command.cwd(here);
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let (rows, cols) = (24, 80);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let session = Arc::new(Session {
            parser: Mutex::new(vt100::Parser::new(nonzero(rows), nonzero(cols), SCROLLBACK)),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            code: Mutex::new(None),
            size: Mutex::new((rows, cols)),
        });
        // A weak reference: dropping the session ends the command, which
        // ends the read.
        let weak: Weak<Session> = Arc::downgrade(&session);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let Some(session) = weak.upgrade() else { break };
                        let reply = match session.parser.lock() {
                            Ok(mut parser) => {
                                parser.process(&buffer[..n]);
                                // Shells ask where the cursor is, and wait.
                                buffer[..n].windows(4).any(|w| w == b"\x1b[6n").then(|| {
                                    let (row, col) = parser.screen().cursor_position();
                                    format!("\x1b[{};{}R", row + 1, col + 1)
                                })
                            }
                            Err(_) => None,
                        };
                        if let Some(reply) = reply {
                            session.write(reply.as_bytes());
                        }
                        OUTPUT.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if let Some(session) = weak.upgrade() {
                let code = session.wait();
                if let Ok(mut c) = session.code.lock() {
                    *c = Some(code);
                }
            }
            OUTPUT.fetch_add(1, Ordering::Relaxed);
            on_end();
        });
        Ok(session)
    }

    /// The code of a command whose output is over, asked without holding
    /// the lock while it still runs.
    fn wait(&self) -> u32 {
        for _ in 0..500 {
            if let Ok(mut child) = self.child.lock() {
                match child.try_wait() {
                    Ok(Some(status)) => return status.exit_code(),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        1
    }

    pub fn code(&self) -> Option<u32> {
        self.code.lock().ok().and_then(|c| *c)
    }

    /// Sends bytes to the command, as typed.
    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
        // Typing brings the view back to the live screen.
        if let Ok(mut parser) = self.parser.lock() {
            if parser.screen().scrollback() > 0 {
                parser.screen_mut().set_scrollback(0);
            }
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(2));
        let Ok(mut size) = self.size.lock() else {
            return;
        };
        if *size == (rows, cols) {
            return;
        }
        *size = (rows, cols);
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        if let Ok(mut parser) = self.parser.lock() {
            parser.set_size(nonzero(rows), nonzero(cols));
        }
    }

    pub fn scroll(&self, lines: i32) {
        if let Ok(mut parser) = self.parser.lock() {
            let screen = parser.screen_mut();
            let at = screen.scrollback() as i64 + lines as i64;
            screen.set_scrollback(at.max(0) as usize);
        }
    }

    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        self.parser.lock().ok().map(|parser| f(parser.screen()))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let ended = self.code().is_some();
        if let Ok(mut child) = self.child.lock() {
            if !ended {
                // The command leads a group of its own: what it started ends
                // with it.
                #[cfg(unix)]
                if let Some(pid) = child.process_id() {
                    // SAFETY: a signal to a process group has no memory effect.
                    unsafe { libc::killpg(pid as i32, libc::SIGTERM) };
                }
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_command_keeps_its_output_and_its_code_after_its_end() {
        let (said, heard) = std::sync::mpsc::channel();
        let session = Session::spawn(
            "sh",
            &["-c".into(), "echo built; exit 3".into()],
            None,
            move || {
                let _ = said.send(());
            },
        )
        .unwrap();
        heard
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(session.code(), Some(3));
        assert!(session
            .with_screen(|s| s.contents().contains("built"))
            .unwrap());
    }
}
