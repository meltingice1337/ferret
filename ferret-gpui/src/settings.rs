//! The app's settings, as ferret-core stores them (the same JSON the web app reads and writes),
//! with typed reads for what the window uses. Saved on every change.

use serde_json::{Value, json};

use crate::sortgroup::ViewSettings;
use crate::theme::{self, Backdrop};

pub struct Settings(pub Value);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Column {
    Modified,
    Type,
    Size,
}

impl Column {
    pub fn key(self) -> &'static str {
        match self {
            Column::Modified => "modified",
            Column::Type => "type",
            Column::Size => "size",
        }
    }
    pub fn default_width(self) -> f32 {
        match self {
            Column::Modified => 150.,
            Column::Type => 140.,
            Column::Size => 90.,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let v = ferret_core::app::with_db(|c| ferret_core::db::load_settings(c)).unwrap_or_else(ferret_core::db::default_settings);
        Settings(v)
    }

    pub fn bool(&self, key: &str, default: bool) -> bool {
        self.0[key].as_bool().unwrap_or(default)
    }

    pub fn str(&self, key: &str) -> &str {
        self.0[key].as_str().unwrap_or("")
    }

    pub fn strings(&self, key: &str) -> Vec<String> {
        self.0[key].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect()
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.0[key] = value;
        self.save();
    }

    pub fn save(&self) {
        let _ = ferret_core::browse::save_settings(self.0.clone());
    }

    pub fn global_view(&self) -> ViewSettings {
        ViewSettings::from_json(&self.0["globalView"])
    }

    pub fn dark(&self) -> bool {
        match self.str("theme") {
            "light" => false,
            "auto" | "system" => crate::settings::system_dark(),
            _ => true,
        }
    }

    pub fn accent(&self) -> u32 {
        theme::parse_hex(self.str("accent")).unwrap_or(theme::DEFAULT_ACCENT)
    }

    pub fn backdrop(&self) -> Backdrop {
        match self.str("backdrop") {
            "acrylic" => Backdrop::Acrylic,
            "solid" => Backdrop::Solid,
            _ => Backdrop::Mica,
        }
    }

    pub fn apply_theme(&self) {
        theme::set(
            self.accent(),
            self.dark(),
            self.backdrop(),
            self.str("density") == "compact",
            self.bool("animations", true) && windows_animates(),
        );
    }

    pub fn column_width(&self, c: Column) -> f32 {
        self.0["columnWidths"][c.key()].as_f64().map(|v| v as f32).unwrap_or(c.default_width()).max(64.)
    }

    pub fn set_column_width(&mut self, c: Column, w: Option<f32>) {
        if !self.0["columnWidths"].is_object() {
            self.0["columnWidths"] = json!({});
        }
        match w {
            Some(w) => self.0["columnWidths"][c.key()] = json!(w.round()),
            None => {
                if let Some(o) = self.0["columnWidths"].as_object_mut() {
                    o.remove(c.key());
                }
            }
        }
        self.save();
    }

    pub fn column_hidden(&self, c: Column) -> bool {
        self.strings("hiddenColumns").iter().any(|k| k == c.key())
    }

    pub fn toggle_column(&mut self, c: Column) {
        let mut list = self.strings("hiddenColumns");
        if let Some(i) = list.iter().position(|k| k == c.key()) {
            list.remove(i);
        } else {
            list.push(c.key().into());
        }
        self.set("hiddenColumns", json!(list));
    }

    pub fn hidden_place(&self, id: &str) -> bool {
        self.strings("hiddenPlaces").iter().any(|k| k == id)
    }

    pub fn hidden_section(&self, id: &str) -> bool {
        self.strings("hiddenSections").iter().any(|k| k == id)
    }

    pub fn toggle_in(&mut self, list_key: &str, id: &str) {
        let mut list = self.strings(list_key);
        if let Some(i) = list.iter().position(|k| k == id) {
            list.remove(i);
        } else {
            list.push(id.into());
        }
        self.set(list_key, json!(list));
    }

    pub fn saved_searches(&self) -> Vec<(String, String)> {
        self.0["savedSearches"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| Some((s["name"].as_str()?.to_string(), s["query"].as_str()?.to_string())))
            .collect()
    }

    pub fn network_places(&self, key: &str) -> Vec<(String, String)> {
        self.0[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| Some((s["path"].as_str()?.to_string(), s["name"].as_str().unwrap_or("").to_string())))
            .collect()
    }
}

/// Windows' app theme (Settings → Personalization → Colors).
pub fn system_dark() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut v: u32 = 0;
    let mut n = std::mem::size_of::<u32>() as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut n),
        )
    };
    !(ok.is_ok() && v == 1)
}

/// Windows' "Animation effects" (Accessibility → Visual effects).
pub fn windows_animates() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut on = windows::core::BOOL(1);
    let _ = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    on.as_bool()
}

pub const ACCENTS: [(&str, u32); 10] = [
    ("Rose", 0xe2457f),
    ("Magenta", 0xc026d3),
    ("Violet", 0x7c5cff),
    ("Blue", 0x2f7dff),
    ("Cyan", 0x06b6d4),
    ("Emerald", 0x10b981),
    ("Amber", 0xf59e0b),
    ("Orange", 0xf97316),
    ("Red", 0xef4444),
    ("Slate", 0x94a3b8),
];
