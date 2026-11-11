//! Updates on screen, as the Tauri app's UpdateCard.tsx: a card with the job cards ("Ferret X is
//! available", then its download), "What's new" (the changes, where the update starts), a dot by
//! the version in the sidebar, and "Check for updates" in Settings. The work is update.rs's.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::{AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, relative};

use crate::theme as t;
use crate::ui::{self, icon};
use crate::update::{self, Offer};
use crate::view::Ferret;

/// The first check, after start-up has settled, then every few hours.
const FIRST_CHECK: Duration = Duration::from_secs(15);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    Available,
    Downloading,
    Installing,
    Error,
}

#[derive(Default)]
pub struct UpdateState {
    pub phase: Phase,
    pub offer: Option<Offer>,
    /// Downloaded so far and in all (bytes), written by the download's thread.
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    pub error: String,
    /// When it last asked (for Settings).
    pub checked_at: Option<chrono::DateTime<chrono::Local>>,
}

impl UpdateState {
    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Checking | Phase::Downloading | Phase::Installing)
    }
}

impl Ferret {
    /// Checks for updates on its own: only an installed copy (a build run from `target\` would
    /// offer to install over itself), or one pointed at a test feed. Once per process.
    pub fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        static STARTED: std::sync::Once = std::sync::Once::new();
        let wanted = crate::appdata::real_install() || std::env::var_os("FERRET_UPDATE_FEED").is_some();
        if !wanted || STARTED.is_completed() {
            return;
        }
        STARTED.call_once(|| {});
        // After an update the search service (FerretIndexer) may be older than this app: one that
        // can update itself is asked to, quietly (from the signed ferret-indexer.exe beside this
        // one); an older one needs the installer, which the user is offered (src/lib/updater.ts's
        // checkIndexer, as the app did).
        if crate::appdata::real_install() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(FIRST_CHECK / 3).await;
                let outdated = cx.background_executor().spawn(async { ferret_core::rt::block_on(ferret_core::search::search_service_outdated()) }).await;
                if outdated == Ok(true) {
                    log::info!("search service is out of date");
                    this.update(cx, |f, cx| {
                        f.toast_action(
                            "The drive index service is out of date",
                            "Update…",
                            crate::view::action(|f, _, cx| {
                                let task = cx.background_executor().spawn(async { ferret_core::rt::block_on(ferret_core::search::search_install_service()) });
                                cx.spawn(async move |this, cx| {
                                    let r = task.await;
                                    this.update(cx, |f, cx| match r {
                                        Ok(()) => f.toast("The drive index service is updated", cx),
                                        Err(e) => f.toast_error(e, cx),
                                    })
                                    .ok();
                                })
                                .detach();
                                f.toast("Updating the drive index service…", cx);
                            }),
                            cx,
                        )
                    })
                    .ok();
                }
            })
            .detach();
        }
        cx.spawn(async move |this, cx| {
            let mut wait = FIRST_CHECK;
            loop {
                cx.background_executor().timer(wait).await;
                if this.update(cx, |f, cx| f.check_for_update(false, cx)).is_err() {
                    break;
                }
                wait = CHECK_EVERY;
            }
        })
        .detach();
    }

    /// Asks the feed. `manual` (Settings' button): says when it's up to date or couldn't ask,
    /// and offers a version even if "Later" hid it.
    pub fn check_for_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update.busy() || (!manual && self.update.phase == Phase::Error) {
            return;
        }
        self.update.phase = Phase::Checking;
        cx.notify();
        let task = cx.background_executor().spawn(async move { update::check() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |f, cx| {
                f.update.checked_at = Some(chrono::Local::now());
                match result {
                    Ok(Some(offer)) => {
                        log::info!("update check: {} available", offer.version);
                        if manual {
                            f.settings.set("updateDismissed", serde_json::Value::String(String::new()));
                        }
                        f.update.offer = Some(offer);
                        f.update.phase = Phase::Available;
                    }
                    Ok(None) => {
                        log::info!("update check: up to date");
                        f.update.offer = None;
                        f.update.phase = Phase::Idle;
                        if manual {
                            f.toast("Ferret is up to date", cx);
                        }
                    }
                    Err(e) => {
                        log::warn!("update check failed: {e}");
                        f.update.phase = if f.update.offer.is_some() { Phase::Available } else { Phase::Idle };
                        if manual {
                            f.toast_error(format!("Couldn't check for updates: {e}"), cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Downloads, checks and runs the installer; Ferret closes for it and starts again after.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.update.offer.clone() else { return };
        if matches!(self.update.phase, Phase::Downloading | Phase::Installing) {
            return;
        }
        log::info!("installing update {}", offer.version);
        self.update.phase = Phase::Downloading;
        self.update.error.clear();
        let (done, total) = (self.update.done.clone(), self.update.total.clone());
        done.store(0, Ordering::Relaxed);
        total.store(0, Ordering::Relaxed);
        let task = cx.background_executor().spawn(async move {
            update::download(&offer, &|d, t| {
                done.store(d, Ordering::Relaxed);
                total.store(t, Ordering::Relaxed);
            })
        });
        // The card's bar follows the download.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let going = this.update(cx, |f, cx| {
                    cx.notify();
                    f.update.phase == Phase::Downloading
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let downloaded = task.await;
            let installer = this
                .update(cx, |f, cx| {
                    let r = downloaded.and_then(|path| update::install(&path).map(|_| path));
                    match &r {
                        Ok(_) => f.update.phase = Phase::Installing,
                        Err(e) => {
                            log::error!("update install failed: {e}");
                            f.update.phase = Phase::Error;
                            f.update.error = e.clone();
                        }
                    }
                    cx.notify();
                    r.is_ok()
                })
                .unwrap_or(false);
            // The installer waits for Ferret to close, then starts it again.
            if installer {
                cx.background_executor().timer(Duration::from_millis(600)).await;
                let _ = cx.update(|cx| cx.quit());
            }
        })
        .detach();
    }

    /// "Later": hidden until a newer version comes.
    pub fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        if let Some(o) = &self.update.offer {
            self.settings.set("updateDismissed", serde_json::Value::String(o.version.clone()));
        }
        if self.update.phase == Phase::Error {
            self.update.phase = Phase::Available;
        }
        cx.notify();
    }

    pub fn show_whats_new(&mut self, cx: &mut Context<Self>) {
        if self.update.offer.is_some() {
            self.dialog = Some(crate::dialogs::Dialog::WhatsNew);
            cx.notify();
        }
    }

    /// A version is on offer (the sidebar's dot).
    pub fn update_waiting(&self) -> Option<&str> {
        self.update.offer.as_ref().filter(|_| matches!(self.update.phase, Phase::Available | Phase::Error)).map(|o| o.version.as_str())
    }

    /// Settings' line about updates.
    pub fn update_status(&self) -> String {
        match self.update.phase {
            Phase::Checking => "Checking…".into(),
            Phase::Downloading | Phase::Installing => format!("Updating to {}…", self.update.offer.as_ref().map(|o| o.version.as_str()).unwrap_or("")),
            _ if self.update.offer.is_some() => format!("Version {} is available", self.update.offer.as_ref().unwrap().version),
            _ => match self.update.checked_at {
                Some(at) => format!("Up to date · checked {}", at.format("%H:%M")),
                None if crate::appdata::installed() => "Checks for updates every few hours".into(),
                None => "A development build: it checks only when asked".into(),
            },
        }
    }

    /// The card with the job cards: "Ferret X is available", the download, or why it failed.
    pub fn update_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let u = &self.update;
        let offer = u.offer.as_ref()?;
        let busy = matches!(u.phase, Phase::Downloading | Phase::Installing);
        let dismissed = self.settings.str("updateDismissed") == offer.version;
        let visible = busy || (!dismissed && matches!(u.phase, Phase::Available | Phase::Error));
        if !visible || matches!(self.dialog, Some(crate::dialogs::Dialog::WhatsNew)) {
            return None;
        }
        let (done, total) = (u.done.load(Ordering::Relaxed), u.total.load(Ordering::Relaxed));
        let frac = if u.phase == Phase::Downloading && total > 0 { done as f32 / total as f32 } else { -1. };
        let title = match u.phase {
            Phase::Downloading => format!("Downloading Ferret {}…", offer.version),
            Phase::Installing => format!("Installing Ferret {}…", offer.version),
            Phase::Error => format!("Couldn't update to {}", offer.version),
            _ => format!("Ferret {} is available", offer.version),
        };
        let changes = offer.new_notes().lines().filter(|l| l.trim_start().starts_with("- ")).count();
        let sub: String = match u.phase {
            Phase::Downloading if total > 0 => format!("{:.0}% · {} of {}", frac * 100., crate::kinds::format_size(done), crate::kinds::format_size(total)),
            Phase::Downloading => crate::kinds::format_size(done),
            Phase::Installing => "Ferret will close and start again".into(),
            Phase::Error => u.error.clone(),
            _ if changes > 0 => format!("You have {} · {} change{}", env!("CARGO_PKG_VERSION"), changes, if changes == 1 { "" } else { "s" }),
            _ => format!("You have {}", env!("CARGO_PKG_VERSION")),
        };
        let error = u.phase == Phase::Error;
        Some(
            div()
                .id("update-card")
                .role(gpui::Role::Group)
                .aria_label(title.clone())
                .p(px(12.))
                .rounded(px(10.))
                .bg(t::raised())
                .border_1()
                .border_color(if error { t::warn() } else { t::line2() })
                .shadow(t::shadow())
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon(if busy { "loader-circle" } else if error { "triangle-alert" } else { "sparkles" }, 15., if error { t::warn() } else { t::accent_text() }))
                        .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(title))
                        .when(!busy, |d| {
                            d.child(
                                div()
                                    .id("update-later-x")
                                    .p(px(3.))
                                    .rounded(px(5.))
                                    .hover(|s| s.bg(t::press()))
                                    .tooltip(ui::tip("Later"))
                                    .on_click(cx.listener(|f, _, _, cx| f.dismiss_update(cx)))
                                    .child(icon("x", 13., t::fg3())),
                            )
                        }),
                )
                .when(busy, |d| {
                    d.child(div().h(px(4.)).rounded(px(4.)).bg(t::line2()).overflow_hidden().child(
                        div().h_full().w(relative(if frac >= 0. { frac.clamp(0., 1.) } else { 1. })).rounded(px(4.)).bg(t::accent()).when(frac < 0., |d| d.opacity(0.45)),
                    ))
                })
                .child(ui::note("update-sub", sub).text_size(px(12.)).text_color(if error { t::warn() } else { t::fg3() }))
                .when(!busy, |d| {
                    d.child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .mt(px(4.))
                            .child(ui::button("update-later", "Later", false).on_click(cx.listener(|f, _, _, cx| f.dismiss_update(cx))))
                            .child(if error {
                                ui::button("update-retry", "Try again", true).on_click(cx.listener(|f, _, _, cx| f.install_update(cx)))
                            } else {
                                ui::button("update-whats-new", "What's new", true).on_click(cx.listener(|f, _, _, cx| f.show_whats_new(cx)))
                            }),
                    )
                })
                .into_any_element(),
        )
    }

    /// "What's new in Ferret X": the changes since this copy's version, Later or Update now.
    pub fn whats_new_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(offer) = self.update.offer.clone() else { return div().into_any_element() };
        let released = chrono::NaiveDate::parse_from_str(&offer.date, "%Y-%m-%d").map(|d| d.format("%-d %B %Y").to_string()).unwrap_or_default();
        let blocks = crate::render::markdown(&offer.new_notes());
        let tall = (window.viewport_size().height.as_f32() - 260.).clamp(160., 420.);
        crate::dialogs::modal(format!("What's new in Ferret {}", offer.version).into(), 560., cx)
            .child(
                crate::dialogs::body_div()
                    .child(ui::note(
                        "wn-sub",
                        if released.is_empty() {
                            format!("You have {}.", env!("CARGO_PKG_VERSION"))
                        } else {
                            format!("Released {released} · you have {}.", env!("CARGO_PKG_VERSION"))
                        },
                    )
                    .text_color(t::fg3()))
                    .child(div().h(px(tall)).rounded(px(8.)).bg(t::hover()).border_1().border_color(t::line()).p(px(10.)).child(crate::render::markdown_view(&blocks, "", None, true)))
                    .child(
                        crate::dialogs::actions()
                            .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(t::fg3()).child("Ferret closes, updates and starts again."))
                            .child(ui::button("wn-later", "Later", false).on_click(cx.listener(|f, _, window, cx| {
                                f.close_dialog(window, cx);
                                f.dismiss_update(cx);
                            })))
                            .child(ui::button("wn-update", "Update now", true).on_click(cx.listener(|f, _, window, cx| {
                                f.close_dialog(window, cx);
                                f.install_update(cx);
                            }))),
                    ),
            )
            .into_any_element()
    }
}
