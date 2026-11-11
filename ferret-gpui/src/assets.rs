//! Icons, built into the exe: Lucide's, as Ferret uses them (`icons/<name>.svg`). build.rs lists
//! every file in assets/icons.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let name = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg"));
        Ok(name.and_then(|n| ICONS.iter().find(|(k, _)| *k == n)).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _)| SharedString::from(format!("icons/{name}.svg")))
            .filter(|p| p.starts_with(path))
            .collect())
    }
}
