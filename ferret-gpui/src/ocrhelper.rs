//! PDF work in helper processes: Ferret's own exe started as `ferret.exe --ocr-worker`, by a
//! Ferret window, when a PDF's pages need drawing or a scanned page needs reading. Two at most
//! per window, one for each job (reading a page takes most of a second, and a page waiting to
//! be drawn mustn't wait behind it). Each quits after a while with nothing to do (or when its
//! window closes: its input ends).
//!
//! Why: whatever does this work keeps what it loaded in its process for good. Windows' text
//! recognizer (about 150 MB, measured) can't be unloaded, neither can ONNX Runtime once loaded
//! (the add-on), and Windows' PDF renderer leaves some 60 MB and 80 threads behind after the
//! first PDF. Done in Ferret, all that stayed in Ferret until it closed; done here, Windows gets
//! it back when the helper quits. It also keeps a PDF that crashes the renderer from taking the
//! window with it (the helper is just started again).
//!
//! The helpers run as the user, like Ferret (not in the search service, which runs as SYSTEM: a
//! malicious PDF must not get its rights). The reader is started below normal priority, so
//! reading never competes with the window; the one that draws pages isn't (the preview waits
//! for it).
//!
//! Protocol: a JSON line per request on its input (`Ask`), a JSON line back (`Answer`; pages
//! come as PNGs in base64). Its log lines come on its stderr and go into Ferret's log.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use crate::ocr::Reader;
use crate::pdftext::PdfLine;

/// The argument that makes `ferret.exe` a helper (followed by `DRAWS` for the one drawing pages).
pub const ARG: &str = "--ocr-worker";
const DRAWS: &str = "pages";
/// A page taking longer than this: the helper is stuck and is stopped.
const PAGE_TIMEOUT: Duration = Duration::from_secs(90);

/// Which helper: the one reading scanned pages, or the one drawing pages.
#[derive(Clone, Copy, PartialEq)]
enum Job {
    Read,
    Draw,
}

impl Job {
    /// It quits after this long with nothing to do. The one drawing pages stays a little longer:
    /// stepping through a folder comes back to PDFs, and each start costs a moment.
    fn idle_exit(self) -> Duration {
        Duration::from_secs(if self == Job::Draw { 45 } else { 20 })
    }
    fn name(self) -> &'static str {
        if self == Job::Draw { "PDF pages helper" } else { "text reader" }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "ask", rename_all = "lowercase")]
enum Ask {
    /// A scanned page's text.
    Read { path: String, password: Option<String>, page: u32 },
    /// A PDF opened with the first of `tries` that works (None: no password): its page count,
    /// every page's shape up to `max`, and the first `first` pages drawn `px` wide.
    Pages { path: String, tries: Vec<Option<String>>, px: u32, first: u32, max: u32 },
    /// More of its pages drawn.
    More { path: String, password: Option<String>, which: Vec<u32>, px: u32 },
    /// Whether this password opens it.
    Unlock { path: String, password: String },
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct Answer {
    reader: Option<Reader>,
    lines: Option<Vec<PdfLine>>,
    error: Option<String>,
    addon_problem: Option<String>,
    /// `Pages`: which of the tries opened it, or that none did.
    opened: Option<usize>,
    locked: bool,
    count: u32,
    /// Each page's height over its width.
    shapes: Vec<f32>,
    /// Pages drawn: their number and PNG (base64).
    pages: Vec<(u32, String)>,
    /// `Unlock`: the password is wrong.
    wrong_password: bool,
}

/// Why a helper gave no result: it couldn't be reached (the caller may do the work itself), or
/// it answered that the PDF can't be done.
pub enum Failed {
    Helper(String),
    Pdf(String),
}

/// A PDF's pages as `pages` found them.
pub enum Pages {
    /// None of the passwords tried opens it.
    Locked,
    Opened { with: usize, count: u32, shapes: Vec<f32>, drawn: Vec<(u32, Vec<u8>)> },
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

/// The reader, the one drawing pages.
static HELPERS: [Mutex<Option<Helper>>; 2] = [Mutex::new(None), Mutex::new(None)];

fn start(job: Job) -> Result<Helper, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    command.arg(ARG);
    if job == Job::Draw {
        command.arg(DRAWS);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Not the test driver's script (a helper isn't a window).
        .env_remove("FERRET_GPUI_SCRIPT")
        .env_remove("FERRET_GPUI_SCRIPT_OUT")
        .creation_flags(CREATE_NO_WINDOW | if job == Job::Read { BELOW_NORMAL_PRIORITY_CLASS } else { 0 })
        .spawn()
        .map_err(|e| format!("the {} couldn't start ({e})", job.name()))?;
    let input = child.stdin.take().ok_or("the helper has no input")?;
    let output = child.stdout.take().ok_or("the helper has no output")?;
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
    log::info!(target: "ferret::ocr", "{} started (process {})", job.name(), child.id());
    Ok(Helper { child, input, answers })
}

/// One request to a helper (started if it isn't running, or again if it quit or crashed), and
/// its answer. Err: the helper couldn't be reached or took too long.
fn ask(job: Job, ask: &Ask, what: &str) -> Result<Answer, String> {
    let text = serde_json::to_string(ask).map_err(|e| e.to_string())?;
    let mut slot = HELPERS[job as usize].lock().unwrap_or_else(|e| e.into_inner());
    // Twice at most: a helper that quit (idle) just as this was sent is started again.
    for _ in 0..2 {
        if slot.as_mut().is_none_or(|h| h.child.try_wait().ok().flatten().is_some()) {
            *slot = Some(start(job)?);
        }
        let Some(h) = slot.as_mut() else { continue };
        if writeln!(h.input, "{text}").and_then(|_| h.input.flush()).is_err() {
            *slot = None;
            continue;
        }
        match h.answers.recv_timeout(PAGE_TIMEOUT) {
            Ok(line) => return serde_json::from_str(&line).map_err(|e| format!("the {}'s answer can't be read ({e})", job.name())),
            Err(RecvTimeoutError::Timeout) => {
                log::warn!(target: "ferret::ocr", "the {} took over {} s on {what}: stopped", job.name(), PAGE_TIMEOUT.as_secs());
                *slot = None;
                return Err("it took too long".into());
            }
            // It ended (crashed, or quit idle as this was sent): once more, with a new one.
            Err(RecvTimeoutError::Disconnected) => {
                *slot = None;
            }
        }
    }
    Err(format!("the {} stopped", job.name()))
}

/// Reads one scanned page in the reader.
pub fn read(path: &str, password: Option<&str>, page: u32) -> Result<(Reader, Vec<PdfLine>), String> {
    let a = ask(Job::Read, &Ask::Read { path: path.into(), password: password.map(str::to_string), page }, &format!("page {page} of {path}"))?;
    crate::ocr::set_reported_problem(a.addon_problem.clone());
    match (a.reader, a.lines, a.error) {
        (Some(r), Some(l), _) => Ok((r, l)),
        (_, _, Some(e)) => Err(e),
        _ => Err("the text reader gave no answer".into()),
    }
}

fn pngs(pages: Vec<(u32, String)>) -> Vec<(u32, Vec<u8>)> {
    pages.into_iter().filter_map(|(i, png)| B64.decode(png).ok().map(|bytes| (i, bytes))).collect()
}

fn drawn(path: &str, request: &Ask) -> Result<Answer, Failed> {
    let a = ask(Job::Draw, request, path).map_err(Failed::Helper)?;
    match a.error {
        Some(e) => Err(Failed::Pdf(e)),
        None => Ok(a),
    }
}

/// Opens a PDF in the helper with the first of `tries` that works, and draws its first pages.
pub fn pages(path: &str, tries: &[Option<String>], px: u32, first: u32, max: u32) -> Result<Pages, Failed> {
    let a = drawn(path, &Ask::Pages { path: path.into(), tries: tries.to_vec(), px, first, max })?;
    match a.opened {
        Some(with) => Ok(Pages::Opened { with, count: a.count, shapes: a.shapes, drawn: pngs(a.pages) }),
        None if a.locked => Ok(Pages::Locked),
        None => Err(Failed::Helper("the PDF pages helper gave no answer".into())),
    }
}

/// Draws more of a PDF's pages in the helper.
pub fn more(path: &str, password: Option<&str>, which: &[u32], px: u32) -> Result<Vec<(u32, Vec<u8>)>, Failed> {
    drawn(path, &Ask::More { path: path.into(), password: password.map(str::to_string), which: which.to_vec(), px }).map(|a| pngs(a.pages))
}

/// Whether `password` opens the PDF, tried in the helper.
pub fn unlock(path: &str, password: &str) -> Result<bool, Failed> {
    drawn(path, &Ask::Unlock { path: path.into(), password: password.into() }).map(|a| !a.wrong_password)
}

/// Stops the helpers (the add-on being removed: its DLL is then no longer in use).
pub fn stop() {
    for slot in &HELPERS {
        if let Ok(mut slot) = slot.lock() {
            *slot = None;
        }
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

/// A helper's life: requests answered as they come, until its idle time passes with none or
/// Ferret goes away (its input ends).
pub fn run() {
    let _ = log::set_boxed_logger(Box::new(ToStderr));
    log::set_max_level(log::LevelFilter::Info);
    let job = if std::env::args().any(|a| a == DRAWS) { Job::Draw } else { Job::Read };
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
    // The document read last, kept open while its pages are asked for. (Pages are drawn from a
    // document opened for each request: one kept open would keep its file from being renamed
    // or deleted for as long as the helper lives.)
    let mut open: Option<(String, Option<String>, windows::Data::Pdf::PdfDocument)> = None;
    let mut engines = None;
    let mut out = std::io::stdout().lock();
    while let Ok(line) = asks.recv_timeout(job.idle_exit()) {
        let answer = match serde_json::from_str::<Ask>(&line) {
            Err(e) => Answer { error: Some(format!("a request it can't read ({e})")), ..Default::default() },
            Ok(Ask::Read { path, password, page }) => {
                let reuse = open.as_ref().is_some_and(|(p, pw, _)| *p == path && *pw == password);
                if !reuse {
                    open = crate::render::pdf_open_with(&path, password.as_deref()).ok().map(|d| (path.clone(), password.clone(), d));
                }
                match open.as_ref() {
                    None => Answer { error: Some(format!("{path} couldn't be opened")), ..Default::default() },
                    Some((_, _, doc)) => match crate::ocr::read_page(doc, page, &mut engines) {
                        Ok((reader, lines)) => Answer { reader: Some(reader), lines: Some(lines), ..Default::default() },
                        Err(e) => Answer { error: Some(e), ..Default::default() },
                    },
                }
            }
            Ok(Ask::Pages { path, tries, px, first, max }) => match crate::render::pdf_pages_here(&path, &tries, px, first, max) {
                Ok(None) => Answer { locked: true, ..Default::default() },
                Ok(Some((with, count, shapes, drawn))) => {
                    Answer { opened: Some(with), count, shapes, pages: drawn.into_iter().map(|(i, png)| (i, B64.encode(png))).collect(), ..Default::default() }
                }
                Err(e) => Answer { error: Some(e), ..Default::default() },
            },
            Ok(Ask::More { path, password, which, px }) => match crate::render::pdf_more_here(&path, password.as_deref(), &which, px) {
                Ok(drawn) => Answer { pages: drawn.into_iter().map(|(i, png)| (i, B64.encode(png))).collect(), ..Default::default() },
                Err(e) => Answer { error: Some(e), ..Default::default() },
            },
            Ok(Ask::Unlock { path, password }) => match crate::render::pdf_pages_here(&path, &[Some(password)], 0, 0, 0) {
                Ok(Some(_)) => Answer::default(),
                Ok(None) => Answer { wrong_password: true, ..Default::default() },
                Err(e) => Answer { error: Some(e), ..Default::default() },
            },
        };
        let answer = Answer { addon_problem: crate::ocr::addon_problem(), ..answer };
        let Ok(text) = serde_json::to_string(&answer) else { break };
        if writeln!(out, "{text}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}
