//! Files dropped on the window from other apps: Ferret's own OLE drop target on the webview,
//! in place of Tauri's (`dragDropEnabled: false` in tauri.conf.json).
//!
//! Tauri's drop event only has the paths and the point. Copy or move also depends on what the
//! app the files come from allows: Explorer lets them be moved, but mail clients, browsers,
//! editors and many other apps only offer a copy, and moving their file would take it away from
//! them (Ferret did, on the same drive). Explorer's rule, which the page applies
//! (src/lib/dropop.ts): a drop only moves when the source allows it; then Ctrl copies, Shift
//! moves, otherwise the same drive moves. So the events here (`ferret-drop`) carry the allowed
//! effects and Ctrl/Shift as Windows reports them during the drag (the page gets no key events
//! while another app drags), and whether the drag is Ferret's own coming back in.
//!
//! The page picks the drop folder (it knows what is under the pointer) and tells the cursor
//! effect back with `drop_feedback`. The effect returned from `Drop` is a copy whatever Ferret
//! does: an app told "moved" may delete its original itself, before Ferret's move has run.

use std::sync::atomic::{AtomicU32, Ordering};

/// What the page would do with the drop under the pointer now (`drop_feedback`):
/// 0 nothing, 1 copy, 2 move. Only for the cursor.
static WANT: AtomicU32 = AtomicU32::new(1);

/// The page's answer for the cursor: "copy", "move" or "none".
#[tauri::command]
pub fn drop_feedback(op: String) {
    WANT.store(
        match op.as_str() {
            "copy" => 1,
            "move" => 2,
            _ => 0,
        },
        Ordering::Relaxed,
    );
}

/// DROPEFFECT bits, as the page reads them.
pub const COPY: u32 = 1;
pub const MOVE: u32 = 2;

/// The cursor effect to show for what the page wants, given what the source allows (Windows
/// refuses an effect it doesn't allow). Ferret's own drag back in only offers a copy, but Ferret
/// moves the files itself, so a move shows as a copy there rather than as "can't drop".
fn cursor_effect(want: u32, allowed: u32) -> u32 {
    match want {
        2 if allowed & MOVE != 0 => MOVE,
        1 | 2 => COPY,
        _ => 0,
    }
}

/// What `Drop` reports back to the source: a copy (or nothing, when it doesn't allow a copy),
/// never a move. See the module comment.
fn drop_result(allowed: u32) -> u32 {
    if allowed & COPY != 0 { COPY } else { 0 }
}

#[cfg(windows)]
pub use imp::install;

#[cfg(not(windows))]
pub fn install(_window: &tauri::WebviewWindow) {}

/// The e2e test build only (scripts/e2e): one step of a drag from another app ("enter", "over",
/// "drop" or "leave"), fed to the drop target as Windows would, with the files as a real
/// CF_HDROP data object. `x`/`y` are CSS pixels in the page; `own` makes it count as Ferret's own
/// drag coming back. Returns the effect the target answered: the cursor, or what `Drop` tells
/// the source.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn e2e_drop(window: tauri::WebviewWindow, step: String, paths: Vec<String>, x: f64, y: f64, allowed: u32, ctrl: bool, shift: bool, own: bool) -> Result<u32, String> {
    if !(cfg!(debug_assertions) && ferret_core::integration::e2e()) {
        return Err("only in the e2e test build".into());
    }
    #[cfg(windows)]
    return imp::e2e_drop(&window, &step, &paths, x, y, allowed, ctrl, shift, own);
    #[cfg(not(windows))]
    {
        let _ = (window, step, paths, x, y, allowed, ctrl, shift, own);
        Err("Windows only".into())
    }
}

#[cfg(windows)]
mod imp {
    use std::cell::{Cell, RefCell};
    use std::os::windows::ffi::OsStringExt;
    use std::sync::atomic::Ordering;

    use serde::Serialize;
    use tauri::{Emitter, WebviewWindow};
    use windows::core::{implement, Ref, Result, BOOL};
    use windows::Win32::Foundation::{DRAGDROP_E_INVALIDHWND, HWND, LPARAM, POINT, POINTL};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
    use windows::Win32::System::Ole::{
        IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, DROPEFFECT,
    };
    use windows::Win32::System::SystemServices::{MK_CONTROL, MK_SHIFT, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    use windows::Win32::UI::WindowsAndMessaging::EnumChildWindows;

    use super::{cursor_effect, drop_result, WANT};

    thread_local! {
        /// The target `install` placed (the main thread's), for `e2e_drop`.
        static TARGET: RefCell<Option<IDropTarget>> = const { RefCell::new(None) };
        /// `e2e_drop`'s stand-in for `drag::dragging`.
        static TEST_OWN: Cell<bool> = const { Cell::new(false) };
    }

    /// One `ferret-drop` event. Paths only on "enter" and "drop"; x/y are physical pixels in
    /// the window's client area, like Tauri's event had them.
    #[derive(Serialize, Clone, PartialEq)]
    #[serde(rename_all = "camelCase")]
    struct DropEvent {
        kind: &'static str,
        paths: Vec<String>,
        x: i32,
        y: i32,
        /// DROPEFFECT bits the source allows (1 copy, 2 move, 4 link).
        allowed: u32,
        ctrl: bool,
        shift: bool,
        /// Ferret's own drag out of this window (`drag::dragging`), coming back in.
        own: bool,
    }

    #[implement(IDropTarget)]
    struct Target {
        window: WebviewWindow,
        hwnd: HWND,
        /// The drag over the window has files in it (others are refused).
        files: Cell<bool>,
        allowed: Cell<u32>,
        own: Cell<bool>,
        /// The last "over" sent: Windows calls DragOver on a timer too, with nothing new.
        last: RefCell<Option<DropEvent>>,
    }

    impl Target {
        fn event(&self, kind: &'static str, paths: Vec<String>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL) -> DropEvent {
            let mut p = POINT { x: pt.x, y: pt.y };
            let _ = unsafe { ScreenToClient(self.hwnd, &mut p) };
            DropEvent {
                kind,
                paths,
                x: p.x,
                y: p.y,
                allowed: self.allowed.get(),
                ctrl: keys.0 & MK_CONTROL.0 != 0,
                shift: keys.0 & MK_SHIFT.0 != 0,
                own: self.own.get(),
            }
        }

        fn send(&self, e: DropEvent) {
            if let Err(err) = self.window.emit("ferret-drop", e) {
                log::warn!("drop event: {err}");
            }
        }

        fn cursor(&self) -> DROPEFFECT {
            DROPEFFECT(cursor_effect(WANT.load(Ordering::Relaxed), self.allowed.get()))
        }
    }

    /// The files in a drag (CF_HDROP), or None when it has none (text, a link, ...).
    fn paths_of(data: Ref<'_, IDataObject>) -> Option<Vec<String>> {
        let data = data.as_ref()?;
        let format = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        unsafe {
            let mut medium = data.GetData(&format).ok()?;
            let hdrop = HDROP(medium.u.hGlobal.0);
            let count = DragQueryFileW(hdrop, u32::MAX, None);
            let mut paths = Vec::with_capacity(count as usize);
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, i, Some(&mut buf));
                paths.push(std::ffi::OsString::from_wide(&buf[..len]).to_string_lossy().into_owned());
            }
            ReleaseStgMedium(&mut medium);
            Some(paths)
        }
    }

    #[allow(non_snake_case)]
    impl IDropTarget_Impl for Target_Impl {
        fn DragEnter(&self, data: Ref<'_, IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            let allowed = unsafe { (*effect).0 };
            let paths = paths_of(data).filter(|p| !p.is_empty());
            self.files.set(paths.is_some());
            let Some(paths) = paths else {
                unsafe { *effect = DROPEFFECT(0) };
                return Ok(());
            };
            self.allowed.set(allowed);
            self.own.set(drag::dragging() || TEST_OWN.with(Cell::get));
            // Until the page answers: a copy, or a move when that's all the source allows.
            WANT.store(if allowed & super::COPY == 0 && allowed & super::MOVE != 0 { 2 } else { 1 }, Ordering::Relaxed);
            *self.last.borrow_mut() = None;
            self.send(self.event("enter", paths, keys, pt));
            unsafe { *effect = self.cursor() };
            Ok(())
        }

        fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            if !self.files.get() {
                unsafe { *effect = DROPEFFECT(0) };
                return Ok(());
            }
            self.allowed.set(unsafe { (*effect).0 });
            let e = self.event("over", Vec::new(), keys, pt);
            if self.last.borrow().as_ref() != Some(&e) {
                *self.last.borrow_mut() = Some(e.clone());
                self.send(e);
            }
            unsafe { *effect = self.cursor() };
            Ok(())
        }

        fn DragLeave(&self) -> Result<()> {
            if self.files.replace(false) {
                self.send(DropEvent { kind: "leave", paths: Vec::new(), x: 0, y: 0, allowed: 0, ctrl: false, shift: false, own: false });
            }
            Ok(())
        }

        fn Drop(&self, data: Ref<'_, IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            let allowed = unsafe { (*effect).0 };
            if !self.files.replace(false) {
                unsafe { *effect = DROPEFFECT(0) };
                return Ok(());
            }
            self.allowed.set(allowed);
            let paths = paths_of(data).unwrap_or_default();
            self.send(self.event("drop", paths, keys, pt));
            unsafe { *effect = DROPEFFECT(drop_result(allowed)) };
            Ok(())
        }
    }

    /// Puts Ferret's drop target on the webview's windows, in place of WebView2's own (or
    /// Tauri's). Run on the main thread once the webview exists (the app's `setup`).
    pub fn install(window: &WebviewWindow) {
        let Ok(hwnd) = window.hwnd() else { return };
        let hwnd = HWND(hwnd.0);
        let target: IDropTarget = Target {
            window: window.clone(),
            hwnd,
            files: Cell::new(false),
            allowed: Cell::new(0),
            own: Cell::new(false),
            last: RefCell::new(None),
        }
        .into();
        // WebView2 takes drops in its child windows, so the target goes on each of them.
        let mut children: Vec<HWND> = Vec::new();
        unsafe extern "system" fn collect(child: HWND, list: LPARAM) -> BOOL {
            unsafe { (*(list.0 as *mut Vec<HWND>)).push(child) };
            true.into()
        }
        let _ = unsafe { EnumChildWindows(Some(hwnd), Some(collect), LPARAM(&mut children as *mut Vec<HWND> as isize)) };
        let mut placed = 0;
        for child in children {
            // Not registered is fine (most aren't); a window that's gone is skipped.
            if unsafe { RevokeDragDrop(child) } == Err(DRAGDROP_E_INVALIDHWND.into()) {
                continue;
            }
            if unsafe { RegisterDragDrop(child, &target) }.is_ok() {
                placed += 1;
            }
        }
        if placed == 0 {
            log::warn!("drop target: no webview window to put it on; files can't be dropped in");
        } else {
            log::info!("drop target on {placed} webview window(s)");
        }
        TARGET.with(|t| *t.borrow_mut() = Some(target));
    }

    /// See `super::e2e_drop`. Runs on the main thread (a synchronous command), like OLE's calls.
    #[allow(clippy::too_many_arguments)]
    pub fn e2e_drop(window: &WebviewWindow, step: &str, paths: &[String], x: f64, y: f64, allowed: u32, ctrl: bool, shift: bool, own: bool) -> std::result::Result<u32, String> {
        use windows::Win32::System::SystemServices::MK_LBUTTON;
        let target = TARGET.with(|t| t.borrow().clone()).ok_or("no drop target")?;
        let (sx, sy) = crate::window::screen_point(window, x, y);
        let pt = POINTL { x: sx, y: sy };
        let mut keys = if step == "drop" { 0 } else { MK_LBUTTON.0 };
        if ctrl {
            keys |= MK_CONTROL.0;
        }
        if shift {
            keys |= MK_SHIFT.0;
        }
        let keys = MODIFIERKEYS_FLAGS(keys);
        let mut effect = DROPEFFECT(allowed);
        let data = match step {
            "enter" | "drop" => Some(data_object(paths)?),
            "over" | "leave" => None,
            _ => return Err(format!("unknown step {step}")),
        };
        TEST_OWN.with(|o| o.set(own));
        let done = unsafe {
            match (step, &data) {
                ("enter", Some(data)) => target.DragEnter(data, keys, pt, &mut effect),
                ("drop", Some(data)) => target.Drop(data, keys, pt, &mut effect),
                ("over", _) => target.DragOver(keys, pt, &mut effect),
                _ => target.DragLeave(),
            }
        };
        TEST_OWN.with(|o| o.set(false));
        done.map_err(|e| e.to_string())?;
        Ok(effect.0)
    }

    /// A data object with `paths` as CF_HDROP, like the one an app hands Windows for a drag.
    fn data_object(paths: &[String]) -> std::result::Result<IDataObject, String> {
        use windows::Win32::System::Com::{STGMEDIUM, STGMEDIUM_0};
        use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
        use windows::Win32::UI::Shell::{SHCreateDataObject, DROPFILES};
        let mut names: Vec<u16> = Vec::new();
        for p in paths {
            names.extend(p.encode_utf16());
            names.push(0);
        }
        names.push(0);
        let head = std::mem::size_of::<DROPFILES>();
        unsafe {
            let data: IDataObject = SHCreateDataObject(None, None, None).map_err(|e| e.to_string())?;
            let memory = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, head + names.len() * 2).map_err(|e| e.to_string())?;
            let at = GlobalLock(memory) as *mut u8;
            if at.is_null() {
                return Err("GlobalLock failed".into());
            }
            let files = DROPFILES { pFiles: head as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
            std::ptr::write_unaligned(at as *mut DROPFILES, files);
            std::ptr::copy_nonoverlapping(names.as_ptr() as *const u8, at.add(head), names.len() * 2);
            let _ = GlobalUnlock(memory);
            let format = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
            let medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: memory }, pUnkForRelease: std::mem::ManuallyDrop::new(None) };
            // The data object owns the memory from here.
            data.SetData(&format, &medium, true).map_err(|e| e.to_string())?;
            Ok(data)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_never_shows_an_effect_the_source_refuses() {
        // Explorer allows copy, move and link.
        assert_eq!(cursor_effect(2, 7), MOVE);
        assert_eq!(cursor_effect(1, 7), COPY);
        // Ferret's own drag back in only offers a copy: a move still drops (shown as a copy).
        assert_eq!(cursor_effect(2, COPY), COPY);
        assert_eq!(cursor_effect(0, 7), 0);
    }

    #[test]
    fn drop_never_tells_the_source_its_files_moved() {
        // A source told "moved" may delete its original itself.
        assert_eq!(drop_result(7), COPY);
        assert_eq!(drop_result(COPY | MOVE), COPY);
        assert_eq!(drop_result(COPY), COPY);
        assert_eq!(drop_result(MOVE), 0);
    }
}
