//! Diagnostics shared by everything: the panic hook and log-line helpers. Where the lines go
//! (a rotating file in the app's log folder, %LOCALAPPDATA%\<identifier>\logs\ferret.log) is the
//! app's choice: the Tauri shell's log_setup.rs.

/// Records panics (including the ones jobs and the shell menu catch) before the
/// default hook runs.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string payload>".into());
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        log::error!(target: "panic", "thread '{}' panicked at {at}: {msg}", thread.name().unwrap_or("<unnamed>"));
        previous(info);
    }));
}

/// A list of paths for a log line, capped so bulk operations don't flood the file.
pub fn paths<S: AsRef<str>>(items: &[S]) -> String {
    const SHOWN: usize = 10;
    let mut out = items.iter().take(SHOWN).map(|p| format!("\"{}\"", p.as_ref())).collect::<Vec<_>>().join(", ");
    if items.len() > SHOWN {
        out.push_str(&format!(" … and {} more", items.len() - SHOWN));
    }
    out
}
