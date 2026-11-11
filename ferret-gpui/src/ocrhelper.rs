//! Text recognition in a helper process: Ferret's own exe started as `ferret.exe --ocr-worker`,
//! one per Ferret window, when a scanned page needs reading. It quits after `IDLE_EXIT` with
//! nothing to read (or when its window closes: its input ends).
//!
//! Why: whatever reads text keeps what it loaded in its process for good. Windows' recognizer
//! (about 150 MB, measured) can't be unloaded, and neither can ONNX Runtime once loaded (the
//! add-on). Read in Ferret, they stayed in Ferret until it closed; read here, Windows gets it all
//! back when the helper quits. It also keeps a PDF that crashes the renderer from taking the
//! window with it (the helper is just started again).
//!
//! The helper runs as the user, like Ferret (not in the search service, which runs as SYSTEM: a
//! malicious PDF must not get its rights). It's started below normal priority, so reading never
//! competes with the window.
//!
//! Protocol: a JSON line per page on its input ({"path", "password", "page"}), a JSON line back
//! ({"reader", "lines"} or {"error"}, and "addon_problem"). Its log lines come on its stderr and
//! go into Ferret's log.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use crate::ocr::Reader;
use crate::pdftext::PdfLine;

/// The argument that makes `ferret.exe` the helper.
pub const ARG: &str = "--ocr-worker";
/// The helper quits after this long with nothing to read.
const IDLE_EXIT: Duration = Duration::from_secs(20);
/// A page taking longer than this: the helper is stuck and is stopped.
const PAGE_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(serde::Serialize, serde::Deserialize)]
struct Ask {
    path: String,
    password: Option<String>,
    page: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Answer {
    reader: Option<Reader>,
    lines: Option<Vec<PdfLine>>,
    error: Option<String>,
    addon_problem: Option<String>,
}

// ───────────────────────────── Ferret's side ─────────────────────────────

struct Helper {
    child: Child,
    input: ChildStdin,
    answers: Receiver<String>,
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Its input closing ends it; a stuck one is ended.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static HELPER: Mutex<Option<Helper>> = Mutex::new(None);

fn start() -> Result<Helper, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg(ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Not the test driver's script (a helper isn't a window).
        .env_remove("FERRET_GPUI_SCRIPT")
        .env_remove("FERRET_GPUI_SCRIPT_OUT")
        .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS)
        .spawn()
        .map_err(|e| format!("the text reader couldn't start ({e})"))?;
    let input = child.stdin.take().ok_or("the text reader has no input")?;
    let output = child.stdout.take().ok_or("the text reader has no output")?;
    let errors = child.stderr.take();
    let (tx, answers) = channel();
    std::thread::Builder::new()
        .name("ocr-answers".into())
        .spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    if let Some(errors) = errors {
        let _ = std::thread::Builder::new().name("ocr-log".into()).spawn(move || {
            for line in BufReader::new(errors).lines() {
                let Ok(line) = line else { break };
                log::info!(target: "ferret::ocr", "helper: {line}");
            }
        });
    }
    log::info!(target: "ferret::ocr", "text reader started (process {})", child.id());
    Ok(Helper { child, input, answers })
}

/// Reads one page in the helper (started if it isn't running, or again if it quit or crashed).
pub fn read(path: &str, password: Option<&str>, page: u32) -> Result<(Reader, Vec<PdfLine>), String> {
    let ask = serde_json::to_string(&Ask { path: path.into(), password: password.map(str::to_string), page }).map_err(|e| e.to_string())?;
    let mut slot = HELPER.lock().unwrap_or_else(|e| e.into_inner());
    // Twice at most: a helper that quit (idle) just as this was sent is started again.
    for _ in 0..2 {
        if slot.as_mut().is_none_or(|h| h.child.try_wait().ok().flatten().is_some()) {
            *slot = Some(start()?);
        }
        let Some(h) = slot.as_mut() else { continue };
        if writeln!(h.input, "{ask}").and_then(|_| h.input.flush()).is_err() {
            *slot = None;
            continue;
        }
        match h.answers.recv_timeout(PAGE_TIMEOUT) {
            Ok(line) => {
                let a: Answer = serde_json::from_str(&line).map_err(|e| format!("the text reader's answer can't be read ({e})"))?;
                crate::ocr::set_reported_problem(a.addon_problem.clone());
                return match (a.reader, a.lines, a.error) {
                    (Some(r), Some(l), _) => Ok((r, l)),
                    (_, _, Some(e)) => Err(e),
                    _ => Err("the text reader gave no answer".into()),
                };
            }
            Err(RecvTimeoutError::Timeout) => {
                log::warn!(target: "ferret::ocr", "the text reader took over {} s on page {page} of {path}: stopped", PAGE_TIMEOUT.as_secs());
                *slot = None;
                return Err("reading this page took too long".into());
            }
            // It ended (crashed, or quit idle as this was sent): once more, with a new one.
            Err(RecvTimeoutError::Disconnected) => {
                *slot = None;
            }
        }
    }
    Err("the text reader stopped".into())
}

/// Stops the helper (the add-on being removed: its DLL is then no longer in use).
pub fn stop() {
    if let Ok(mut slot) = HELPER.lock() {
        *slot = None;
    }
}

// ───────────────────────────── the helper ─────────────────────────────

/// Log lines to stderr (Ferret writes them into its log).
struct ToStderr;

impl log::Log for ToStderr {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= if meta.target().starts_with("ferret") { log::Level::Info } else { log::Level::Warn }
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{}", record.args());
        }
    }
    fn flush(&self) {}
}

/// The helper's life: pages read as they're asked for, until `IDLE_EXIT` passes with none or
/// Ferret goes away (its input ends).
pub fn run() {
    let _ = log::set_boxed_logger(Box::new(ToStderr));
    log::set_max_level(log::LevelFilter::Info);
    let _com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
    let (tx, asks) = channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    // The document read last, kept open while its pages are asked for.
    let mut open: Option<(String, Option<String>, windows::Data::Pdf::PdfDocument)> = None;
    let mut engines = None;
    let mut out = std::io::stdout().lock();
    while let Ok(line) = asks.recv_timeout(IDLE_EXIT) {
        let answer = match serde_json::from_str::<Ask>(&line) {
            Err(e) => Answer { error: Some(format!("a request it can't read ({e})")), ..Default::default() },
            Ok(ask) => {
                let reuse = open.as_ref().is_some_and(|(p, pw, _)| *p == ask.path && *pw == ask.password);
                if !reuse {
                    open = crate::render::pdf_open_with(&ask.path, ask.password.as_deref()).ok().map(|d| (ask.path.clone(), ask.password.clone(), d));
                }
                match open.as_ref() {
                    None => Answer { error: Some(format!("{} couldn't be opened", ask.path)), ..Default::default() },
                    Some((_, _, doc)) => match crate::ocr::read_page(doc, ask.page, &mut engines) {
                        Ok((reader, lines)) => Answer { reader: Some(reader), lines: Some(lines), ..Default::default() },
                        Err(e) => Answer { error: Some(e), ..Default::default() },
                    },
                }
            }
        };
        let answer = Answer { addon_problem: crate::ocr::addon_problem(), ..answer };
        let Ok(text) = serde_json::to_string(&answer) else { break };
        if writeln!(out, "{text}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}
