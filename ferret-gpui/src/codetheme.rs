//! The app's code colours (shiki's github-dark-default / github-light-default, VS Code's GitHub
//! themes) as syntect themes. Generated from @shikijs/themes; the scope rules are theirs.

use std::str::FromStr;

use syntect::highlighting::{Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings};

const DARK_FG: &str = "#e6edf3";
const DARK: &[(&str, &str, &str)] = &[
    ("comment, punctuation.definition.comment, string.comment", "#8b949e", ""),
    ("constant.other.placeholder, constant.character", "#ff7b72", ""),
    ("constant, entity.name.constant, variable.other.constant, variable.other.enummember, variable.language, entity", "#79c0ff", ""),
    ("entity.name, meta.export.default, meta.definition.variable", "#ffa657", ""),
    ("variable.parameter.function, meta.jsx.children, meta.block, meta.tag.attributes, entity.name.constant, meta.object.member, meta.embedded.expression", "#e6edf3", ""),
    ("entity.name.function", "#d2a8ff", ""),
    ("entity.name.tag, support.class.component", "#7ee787", ""),
    ("keyword", "#ff7b72", ""),
    ("storage, storage.type", "#ff7b72", ""),
    ("storage.modifier.package, storage.modifier.import, storage.type.java", "#e6edf3", ""),
    ("string, string punctuation.section.embedded source", "#a5d6ff", ""),
    ("support", "#79c0ff", ""),
    ("meta.property-name", "#79c0ff", ""),
    ("variable", "#ffa657", ""),
    ("variable.other", "#e6edf3", ""),
    ("invalid.broken", "#ffa198", "italic"),
    ("invalid.deprecated", "#ffa198", "italic"),
    ("invalid.illegal", "#ffa198", "italic"),
    ("invalid.unimplemented", "#ffa198", "italic"),
    ("carriage-return", "#f0f6fc", "italic underline"),
    ("message.error", "#ffa198", ""),
    ("string variable", "#79c0ff", ""),
    ("source.regexp, string.regexp", "#a5d6ff", ""),
    ("string.regexp.character-class, string.regexp constant.character.escape, string.regexp source.ruby.embedded, string.regexp string.regexp.arbitrary-repitition", "#a5d6ff", ""),
    ("string.regexp constant.character.escape", "#7ee787", "bold"),
    ("support.constant", "#79c0ff", ""),
    ("support.variable", "#79c0ff", ""),
    ("support.type.property-name.json", "#7ee787", ""),
    ("meta.module-reference", "#79c0ff", ""),
    ("punctuation.definition.list.begin.markdown", "#ffa657", ""),
    ("markup.heading, markup.heading entity.name", "#79c0ff", "bold"),
    ("markup.quote", "#7ee787", ""),
    ("markup.italic", "#e6edf3", "italic"),
    ("markup.bold", "#e6edf3", "bold"),
    ("markup.underline", "", "underline"),
    ("markup.strikethrough", "", "strikethrough"),
    ("markup.inline.raw", "#79c0ff", ""),
    ("markup.deleted, meta.diff.header.from-file, punctuation.definition.deleted", "#ffa198", ""),
    ("punctuation.section.embedded", "#ff7b72", ""),
    ("markup.inserted, meta.diff.header.to-file, punctuation.definition.inserted", "#7ee787", ""),
    ("markup.changed, punctuation.definition.changed", "#ffa657", ""),
    ("markup.ignored, markup.untracked", "#161b22", ""),
    ("meta.diff.range", "#d2a8ff", "bold"),
    ("meta.diff.header", "#79c0ff", ""),
    ("meta.separator", "#79c0ff", "bold"),
    ("meta.output", "#79c0ff", ""),
    ("brackethighlighter.tag, brackethighlighter.curly, brackethighlighter.round, brackethighlighter.square, brackethighlighter.angle, brackethighlighter.quote", "#8b949e", ""),
    ("brackethighlighter.unmatched", "#ffa198", ""),
    ("constant.other.reference.link, string.other.link", "#a5d6ff", ""),
];

const LIGHT_FG: &str = "#1f2328";
const LIGHT: &[(&str, &str, &str)] = &[
    ("comment, punctuation.definition.comment, string.comment", "#6e7781", ""),
    ("constant.other.placeholder, constant.character", "#cf222e", ""),
    ("constant, entity.name.constant, variable.other.constant, variable.other.enummember, variable.language, entity", "#0550ae", ""),
    ("entity.name, meta.export.default, meta.definition.variable", "#953800", ""),
    ("variable.parameter.function, meta.jsx.children, meta.block, meta.tag.attributes, entity.name.constant, meta.object.member, meta.embedded.expression", "#1f2328", ""),
    ("entity.name.function", "#8250df", ""),
    ("entity.name.tag, support.class.component", "#116329", ""),
    ("keyword", "#cf222e", ""),
    ("storage, storage.type", "#cf222e", ""),
    ("storage.modifier.package, storage.modifier.import, storage.type.java", "#1f2328", ""),
    ("string, string punctuation.section.embedded source", "#0a3069", ""),
    ("support", "#0550ae", ""),
    ("meta.property-name", "#0550ae", ""),
    ("variable", "#953800", ""),
    ("variable.other", "#1f2328", ""),
    ("invalid.broken", "#82071e", "italic"),
    ("invalid.deprecated", "#82071e", "italic"),
    ("invalid.illegal", "#82071e", "italic"),
    ("invalid.unimplemented", "#82071e", "italic"),
    ("carriage-return", "#f6f8fa", "italic underline"),
    ("message.error", "#82071e", ""),
    ("string variable", "#0550ae", ""),
    ("source.regexp, string.regexp", "#0a3069", ""),
    ("string.regexp.character-class, string.regexp constant.character.escape, string.regexp source.ruby.embedded, string.regexp string.regexp.arbitrary-repitition", "#0a3069", ""),
    ("string.regexp constant.character.escape", "#116329", "bold"),
    ("support.constant", "#0550ae", ""),
    ("support.variable", "#0550ae", ""),
    ("support.type.property-name.json", "#116329", ""),
    ("meta.module-reference", "#0550ae", ""),
    ("punctuation.definition.list.begin.markdown", "#953800", ""),
    ("markup.heading, markup.heading entity.name", "#0550ae", "bold"),
    ("markup.quote", "#116329", ""),
    ("markup.italic", "#1f2328", "italic"),
    ("markup.bold", "#1f2328", "bold"),
    ("markup.underline", "", "underline"),
    ("markup.strikethrough", "", "strikethrough"),
    ("markup.inline.raw", "#0550ae", ""),
    ("markup.deleted, meta.diff.header.from-file, punctuation.definition.deleted", "#82071e", ""),
    ("punctuation.section.embedded", "#cf222e", ""),
    ("markup.inserted, meta.diff.header.to-file, punctuation.definition.inserted", "#116329", ""),
    ("markup.changed, punctuation.definition.changed", "#953800", ""),
    ("markup.ignored, markup.untracked", "#eaeef2", ""),
    ("meta.diff.range", "#8250df", "bold"),
    ("meta.diff.header", "#0550ae", ""),
    ("meta.separator", "#0550ae", "bold"),
    ("meta.output", "#0550ae", ""),
    ("brackethighlighter.tag, brackethighlighter.curly, brackethighlighter.round, brackethighlighter.square, brackethighlighter.angle, brackethighlighter.quote", "#57606a", ""),
    ("brackethighlighter.unmatched", "#82071e", ""),
    ("constant.other.reference.link, string.other.link", "#0a3069", ""),
];

fn color(hex: &str) -> Option<Color> {
    let h = hex.trim_start_matches('#');
    let v = u32::from_str_radix(h.get(..6)?, 16).ok()?;
    let a = h.get(6..8).and_then(|a| u8::from_str_radix(a, 16).ok()).unwrap_or(255);
    Some(Color { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8, a })
}

fn style(s: &str) -> Option<FontStyle> {
    if s.is_empty() {
        return None;
    }
    let mut f = FontStyle::empty();
    for w in s.split_whitespace() {
        match w {
            "bold" => f |= FontStyle::BOLD,
            "italic" => f |= FontStyle::ITALIC,
            "underline" => f |= FontStyle::UNDERLINE,
            _ => {}
        }
    }
    Some(f)
}

/// The GitHub theme for the dark or light look.
pub fn github(dark: bool) -> Theme {
    let (fg, rules) = if dark { (DARK_FG, DARK) } else { (LIGHT_FG, LIGHT) };
    Theme {
        name: Some(if dark { "github-dark-default" } else { "github-light-default" }.into()),
        settings: ThemeSettings { foreground: color(fg), ..Default::default() },
        scopes: rules
            .iter()
            .filter_map(|(sel, fg, fs)| {
                Some(ThemeItem { scope: ScopeSelectors::from_str(sel).ok()?, style: StyleModifier { foreground: color(fg), background: None, font_style: style(fs) } })
            })
            .collect(),
        ..Default::default()
    }
}
