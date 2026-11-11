//! Servers and network folders (RemoteDialogs.tsx): the SSH connection form (from ~/.ssh/config
//! or by hand, with a connection test), adding a NAS or shared folder, and signing in to one.

use gpui::{AnyElement, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use serde_json::json;

use crate::dialogs::{Dialog, actions, body_div, focus_later, modal};
use crate::input::TextInput;
use crate::theme as t;
use crate::ui;
use crate::view::Ferret;

pub struct ServerForm {
    pub id: Option<String>,
    pub name: Entity<TextInput>,
    pub host: Entity<TextInput>,
    pub port: Entity<TextInput>,
    pub user: Entity<TextInput>,
    pub key: Entity<TextInput>,
    pub start: Entity<TextInput>,
    /// "auto" | "key" | "password"
    pub auth: &'static str,
    pub search: bool,
    pub alias: Option<String>,
    pub hosts: Vec<ferret_core::remote::config::HostConfig>,
    pub test: Option<Result<String, String>>,
    pub testing: bool,
}

pub struct NetworkForm {
    pub address: Entity<TextInput>,
    pub name: Entity<TextInput>,
    pub error: Option<String>,
}

fn field(cx: &mut Context<Ferret>, placeholder: &str, text: &str) -> Entity<TextInput> {
    let (p, t) = (placeholder.to_string(), text.to_string());
    cx.new(|cx| {
        let mut i = TextInput::new(p, cx).sized(13., 20.);
        i.set_text(&t, cx);
        i
    })
}

impl Ferret {
    /// Connects to a saved server (asking for a password or a new host key if it must) and
    /// opens where it opens in ("Opens in", else the home folder), as the app's `openConnection`.
    pub fn open_connection(&mut self, id: String, new_tab: bool, window: &mut Window, cx: &mut Context<Self>) {
        let name = ferret_core::remote::name_of(&id);
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::remote::remote_connect(id)) });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            this.update_in(cx, |f, _, cx| match r {
                Ok(path) if new_tab => f.new_tab(path, cx),
                Ok(path) => f.go(path, cx),
                Err(e) if e == "Cancelled" => {}
                Err(e) => f.toast_error(format!("Couldn't connect to {name}: {e}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn open_server_form(&mut self, id: Option<String>, _window: &mut Window, cx: &mut Context<Self>) {
        let existing = id.as_ref().and_then(|id| ferret_core::remote::remote_connections().into_iter().find(|c| &c.id == id));
        let c = existing.as_ref();
        let name = field(cx, "My server", c.map(|c| c.name.as_str()).unwrap_or(""));
        let host = field(cx, "server.example.com or 192.168.1.10", c.map(|c| c.host.as_str()).unwrap_or(""));
        let port = field(cx, "22", &c.map(|c| c.port.to_string()).unwrap_or_else(|| "22".into()));
        let user = field(cx, "user", c.map(|c| c.user.as_str()).unwrap_or(""));
        let key = field(cx, "C:\\Users\\me\\.ssh\\id_ed25519", c.and_then(|c| c.identity_file.as_deref()).unwrap_or(""));
        let start = field(cx, "/home/me", c.and_then(|c| c.start_dir.as_deref()).unwrap_or(""));
        let auth: &'static str = match c.map(|c| c.auth.as_str()) {
            Some("key") => "key",
            Some("password") => "password",
            _ => "auto",
        };
        focus_later(host.clone(), cx);
        self.dialog = Some(Dialog::Server(Box::new(ServerForm {
            id,
            name,
            host,
            port,
            user,
            key,
            start,
            auth,
            // Off for a new server, as in the app: searching runs `find` over its home folder
            // and the usual app folders, which the user turns on knowingly.
            search: c.map(|c| c.search).unwrap_or(false),
            alias: c.and_then(|c| c.ssh_alias.clone()),
            hosts: ferret_core::remote::remote_config_hosts(),
            test: None,
            testing: false,
        })));
        cx.notify();
    }

    pub fn open_network_form(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let address = field(cx, "\\\\nas\\media, smb://nas/media or 192.168.1.20", "");
        let name = field(cx, "Media on the NAS", "");
        focus_later(address.clone(), cx);
        self.dialog = Some(Dialog::NetworkAdd(Box::new(NetworkForm { address, name, error: None })));
        cx.notify();
    }

    fn connection_from_form(&self, f: &ServerForm, cx: &Context<Self>) -> ferret_core::remote::config::Connection {
        let text = |e: &Entity<TextInput>| e.read(cx).text().trim().to_string();
        let opt = |s: String| (!s.is_empty()).then_some(s);
        serde_json::from_value(json!({
            "id": f.id.clone().unwrap_or_default(),
            "name": text(&f.name),
            "kind": "ssh",
            "host": text(&f.host),
            "port": text(&f.port).parse::<u16>().unwrap_or(22),
            "user": text(&f.user),
            "sshAlias": f.alias,
            "auth": f.auth,
            "identityFile": opt(text(&f.key)),
            "startDir": opt(text(&f.start)),
            "search": f.search,
        }))
        .expect("a connection")
    }

    pub fn submit_remote_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.dialog.as_ref() {
            Some(Dialog::Server(f)) => {
                let conn = self.connection_from_form(f, cx);
                if conn.host.is_empty() {
                    return;
                }
                let is_new = f.id.is_none();
                let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::remote::remote_save_connection(conn)) });
                cx.spawn_in(window, async move |this, cx| {
                    let r = task.await;
                    this.update_in(cx, |f, window, cx| match r {
                        Ok(list) => {
                            f.close_dialog(window, cx);
                            // A new server (saved last, with its id) opens right away; an edited
                            // one stays as it is (Save used to open the last server in the list).
                            if let Some(c) = list.last().filter(|_| is_new) {
                                f.open_connection(c.id.clone(), false, window, cx);
                            }
                        }
                        Err(e) => f.toast_error(e, cx),
                    })
                    .ok();
                })
                .detach();
            }
            Some(Dialog::NetworkAdd(nf)) => {
                let raw = nf.address.read(cx).text().trim().to_string();
                let name = nf.name.read(cx).text().trim().to_string();
                // `\\nas\media`, `//nas/media`, `smb://nas/media`, an IP or a name.
                let mut path = raw.replace('/', "\\");
                if let Some(rest) = path.strip_prefix("smb:\\\\") {
                    path = format!("\\\\{rest}");
                }
                if !path.starts_with("\\\\") {
                    path = format!("\\\\{}", path.trim_start_matches('\\'));
                }
                if path.len() <= 2 {
                    if let Some(Dialog::NetworkAdd(nf)) = self.dialog.as_mut() {
                        nf.error = Some("Type the folder's address".into());
                    }
                    cx.notify();
                    return;
                }
                let mut list: Vec<serde_json::Value> = self.settings.0["networkPlaces"].as_array().cloned().unwrap_or_default();
                list.retain(|v| v["path"].as_str().is_none_or(|p| !p.eq_ignore_ascii_case(&path)));
                let shown = if name.is_empty() { crate::util::folder_name(&path) } else { name };
                list.push(json!({ "path": path, "name": shown }));
                self.settings.set("networkPlaces", serde_json::Value::Array(list));
                self.close_dialog(window, cx);
                self.go(path, cx);
            }
            _ => {}
        }
    }

    fn test_server(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Server(f)) = self.dialog.as_mut() else { return };
        f.testing = true;
        f.test = None;
        let conn = {
            let Some(Dialog::Server(f)) = self.dialog.as_ref() else { return };
            self.connection_from_form(f, cx)
        };
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::remote::remote_test(conn)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| {
                if let Some(Dialog::Server(form)) = f.dialog.as_mut() {
                    form.testing = false;
                    form.test = Some(r.map(|t| match t.arch {
                        Some(a) => format!("Connected · home {} · Linux ({a}): folder sizes and search run on the server", t.home),
                        None => format!("Connected · home {} · files only (SFTP)", t.home),
                    }));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Fills the form from a ~/.ssh/config host.
    fn use_host(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Dialog::Server(f)) = self.dialog.as_mut() else { return };
        let Some(h) = f.hosts.get(ix).cloned() else { return };
        f.alias = Some(h.alias.clone());
        let set = |e: &Entity<TextInput>, s: &str, cx: &mut Context<Self>| e.update(cx, |i, cx| i.set_text(s, cx));
        let (name, host, port, user, key) = (f.name.clone(), f.host.clone(), f.port.clone(), f.user.clone(), f.key.clone());
        set(&name, &h.alias, cx);
        set(&host, h.host_name.as_deref().unwrap_or(&h.alias), cx);
        set(&port, &h.port.unwrap_or(22).to_string(), cx);
        set(&user, h.user.as_deref().unwrap_or(""), cx);
        set(&key, h.identity_files.first().map(String::as_str).unwrap_or(""), cx);
        cx.notify();
    }

    pub fn remote_form_view(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row = |label: &'static str, input: &Entity<TextInput>| {
            div().flex_1().flex().flex_col().gap(px(4.)).child(div().text_size(px(12.)).text_color(t::fg3()).child(label)).child(ui::field(false).child(input.clone()))
        };
        match self.dialog.as_ref() {
            Some(Dialog::Server(f)) => {
                let editing = f.id.is_some();
                let hosts: Vec<(usize, String)> = f.hosts.iter().enumerate().map(|(i, h)| (i, h.alias.clone())).collect();
                let auth = f.auth;
                let search = f.search;
                let test = f.test.clone();
                let testing = f.testing;
                modal(if editing { "Edit server".into() } else { "Add a server".into() }, 560., cx)
                    .child(
                        body_div()
                            .when(!hosts.is_empty(), |d| {
                                d.child(div().flex().flex_col().gap(px(4.)).child(div().text_size(px(12.)).text_color(t::fg3()).child("From ~/.ssh/config")).child(div().flex().flex_wrap().gap(px(6.)).children(hosts.into_iter().map(|(i, alias)| {
                                    ui::status_btn(("ssh-host", i), Some("server"), Some(alias.into()), false).on_click(cx.listener(move |f, _, _, cx| f.use_host(i, cx)))
                                }))))
                            })
                            .child(row("Name", &f.name))
                            .child(div().flex().gap(px(10.)).child(row("Server", &f.host)).child(div().w(px(90.)).child(row("Port", &f.port))))
                            .child(row("User", &f.user))
                            .child(
                                div().flex().flex_col().gap(px(4.)).child(div().text_size(px(12.)).text_color(t::fg3()).child("Sign in with")).child(ui::seg().children(
                                    [("auto", "Keys, then password"), ("key", "A key file"), ("password", "A password")].into_iter().enumerate().map(|(i, (v, l))| {
                                        ui::seg_btn(("auth", i), None, Some(SharedString::from(l)), auth == v).h(px(28.)).px(px(12.)).on_click(cx.listener(move |f, _, _, cx| {
                                            if let Some(Dialog::Server(form)) = f.dialog.as_mut() {
                                                form.auth = v;
                                            }
                                            cx.notify();
                                        }))
                                    }),
                                )),
                            )
                            .when(auth == "key", |d| d.child(row("Key file", &f.key)))
                            .child(row("Opens in", &f.start))
                            .child(ui::checkbox("ssh-search", "Search its files with Ctrl+K", search).on_click(cx.listener(|f, _, _, cx| {
                                if let Some(Dialog::Server(form)) = f.dialog.as_mut() {
                                    form.search = !form.search;
                                }
                                cx.notify();
                            })))
                            .children(test.map(|r| match r {
                                Ok(s) => div().text_size(px(12.)).text_color(t::ok()).child(s),
                                Err(e) => div().text_size(px(12.)).text_color(t::danger()).child(e),
                            }))
                            .child(
                                actions()
                                    .child(ui::button("ssh-test", if testing { "Testing…" } else { "Test connection" }, false).on_click(cx.listener(|f, _, _, cx| f.test_server(cx))))
                                    .child(ui::button("ssh-cancel", "Cancel", false).on_click(cx.listener(|f, _, window, cx| f.close_dialog(window, cx))))
                                    .child(ui::button("ssh-save", if editing { "Save" } else { "Add and connect" }, true).on_click(cx.listener(|f, _, window, cx| f.submit_remote_form(window, cx)))),
                            ),
                    )
                    .into_any_element()
            }
            Some(Dialog::NetworkAdd(nf)) => modal("Add a network folder".into(), 480., cx)
                .child(
                    body_div()
                        .child(div().text_color(t::fg2()).child("A NAS or a folder another computer shares. Ferret asks for a sign-in if it needs one."))
                        .child(row("Address", &nf.address))
                        .child(row("Name", &nf.name))
                        .children(nf.error.clone().map(|e| div().text_color(t::danger()).text_size(px(12.)).child(e)))
                        .child(
                            actions()
                                .child(ui::button("net-cancel", "Cancel", false).on_click(cx.listener(|f, _, window, cx| f.close_dialog(window, cx))))
                                .child(ui::button("net-add", "Add", true).on_click(cx.listener(|f, _, window, cx| f.submit_remote_form(window, cx)))),
                        ),
                )
                .into_any_element(),
            _ => div().into_any_element(),
        }
        .pipe(|card| crate::dialogs::scrim(card, cx))
    }

    /// A network folder that needs a sign-in: user and password (remembered if asked).
    pub fn network_sign_in(&mut self, path: String, cx: &mut Context<Self>) {
        let user = cx.new(|cx| TextInput::new("user", cx).sized(13., 20.).named("User"));
        // The password shows as dots.
        let pass = cx.new(|cx| TextInput::new("", cx).sized(13., 20.).masked(true).named("Password"));
        focus_later(user.clone(), cx);
        let p = path.clone();
        // As the app's NetworkLogin: the server by name, and Windows' reason when the folder gave
        // one (it's in the pane's "Not connected" error), else what kind of account it means.
        let server = path.trim_start_matches('\\').split('\\').next().filter(|s| !s.is_empty()).unwrap_or(&path).to_string();
        let reason = self
            .tabs
            .iter()
            .flat_map(|t| std::iter::once(&t.main).chain(t.split.as_ref()))
            .filter(|pane| crate::util::same(&pane.path, &path))
            .find_map(|pane| pane.error.as_deref().and_then(|e| e.strip_prefix(ferret_core::network::LOGIN_PREFIX)).map(str::trim).filter(|r| !r.is_empty()).map(str::to_string));
        self.dialog = Some(Dialog::Remote {
            id: 0,
            title: format!("Sign in to {server}").into(),
            message: reason
                .unwrap_or_else(|| format!("“{server}” asks for a user name and password: usually an account on that device, not your Windows one."))
                .into(),
            fields: vec![("User".into(), user, false), ("Password".into(), pass, true)],
            fingerprint: None,
            can_remember: true,
            remember: false,
            ok: "Sign in".into(),
            danger: false,
        });
        self.network_pending = Some(p);
        cx.notify();
    }

    /// The network sign-in's answer (the dialog shares the SSH prompt's look).
    pub fn answer_network(&mut self, values: Vec<String>, remember: bool, cx: &mut Context<Self>) {
        let Some(path) = self.network_pending.take() else { return };
        let (user, pass) = (values.first().cloned(), values.get(1).cloned());
        let p2 = path.clone();
        let task = cx.background_executor().spawn(async move { ferret_core::network::network_connect(p2, user, pass, remember) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(_) => f.reload_all(cx),
                Err(e) => {
                    f.toast_error(format!("Sign-in refused: {e}"), cx);
                    f.network_sign_in(path.clone(), cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

#[allow(dead_code)]
fn _w(_: FontWeight) {}
