//! Documents with no renderer of their own (.doc, .rtf, .odt, .ppt, .odp, .ods, .wpd…): their
//! text, read directly from OpenDocument files, or by the text extractors Windows Search uses
//! (IFilter; Office installs them for its formats). Shown as a page of paragraphs.

/// The document's paragraphs (true for headings).
pub fn doc_text(path: &str, ext: &str) -> Result<Vec<(bool, String)>, String> {
    match ext {
        "odt" | "ott" | "odp" | "otp" => return odf_paragraphs(path),
        "rtf" => return std::fs::read(path).map(|b| rtf_text(&String::from_utf8_lossy(&b))).map_err(|e| e.to_string()),
        "doc" | "dot" => {
            if let Ok(p) = doc97_text(path) {
                return Ok(p);
            }
        }
        "ppt" | "pps" | "pot" => {
            if let Ok(p) = ppt97_text(path) {
                return Ok(p);
            }
        }
        _ => {}
    }
    let text = ifilter_text(path, 400_000)?;
    let paras: Vec<(bool, String)> =
        text.replace('\r', "\n").split('\n').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).map(|l| (false, l)).collect();
    if paras.is_empty() { Err("no text".into()) } else { Ok(paras) }
}

fn content_xml(path: &str) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut xml = String::new();
    zip.by_name("content.xml").map_err(|e| e.to_string())?.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    Ok(xml)
}

/// OpenDocument text or slides: content.xml's paragraphs and headings, in order.
fn odf_paragraphs(path: &str) -> Result<Vec<(bool, String)>, String> {
    let xml = content_xml(path)?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element() && matches!(n.tag_name().name(), "p" | "h")) {
        // A paragraph's text, with its spaces and tabs (text:s, text:tab) put back.
        let mut s = String::new();
        for d in n.descendants() {
            if d.is_text() {
                s.push_str(d.text().unwrap_or(""));
            } else if d.is_element() {
                match d.tag_name().name() {
                    "s" => s.push(' '),
                    "tab" => s.push('\t'),
                    "line-break" => s.push('\n'),
                    _ => {}
                }
            }
        }
        if !s.trim().is_empty() {
            out.push((n.tag_name().name() == "h", s.trim().to_string()));
        }
        if out.len() > 4000 {
            break;
        }
    }
    if out.is_empty() { Err("no text".into()) } else { Ok(out) }
}

/// An OpenDocument spreadsheet's first sheet as rows of cells.
pub fn ods_rows(path: &str) -> Result<Vec<Vec<String>>, String> {
    let xml = content_xml(path)?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
    let Some(table) = doc.descendants().find(|n| n.is_element() && n.tag_name().name() == "table") else {
        return Err("no sheet".into());
    };
    let ns = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
    let mut rows: Vec<Vec<String>> = Vec::new();
    for r in table.descendants().filter(|n| n.is_element() && n.tag_name().name() == "table-row") {
        let mut row: Vec<String> = Vec::new();
        for c in r.children().filter(|n| n.is_element() && n.tag_name().name() == "table-cell") {
            let text: String = c.descendants().filter(|d| d.is_text()).map(|d| d.text().unwrap_or("")).collect();
            let repeat: usize = c.attribute((ns, "number-columns-repeated")).and_then(|v| v.parse().ok()).unwrap_or(1).min(60);
            for _ in 0..repeat {
                row.push(text.clone());
            }
        }
        while row.last().is_some_and(|c| c.is_empty()) {
            row.pop();
        }
        let repeat: usize = r.attribute((ns, "number-rows-repeated")).and_then(|v| v.parse().ok()).unwrap_or(1);
        if row.is_empty() && repeat > 50 {
            continue;
        }
        for _ in 0..repeat.min(50) {
            rows.push(row.clone());
        }
        if rows.len() > 5000 {
            break;
        }
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    Ok(rows)
}

/// The text of a file through its IFilter (up to `max` characters).
fn ifilter_text(path: &str, max: usize) -> Result<String, String> {
    use windows::Win32::Storage::IndexServer::*;
    use windows::core::{HSTRING, Interface, PWSTR};
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        let mut raw = std::ptr::null_mut();
        LoadIFilter(&HSTRING::from(path), None, &mut raw).map_err(|e| e.message().to_string())?;
        let filter = IFilter::from_raw(raw);
        let flags = (IFILTER_INIT_CANON_PARAGRAPHS.0
            | IFILTER_INIT_HARD_LINE_BREAKS.0
            | IFILTER_INIT_CANON_HYPHENS.0
            | IFILTER_INIT_CANON_SPACES.0
            | IFILTER_INIT_DISABLE_EMBEDDED.0
            | IFILTER_INIT_INDEXING_ONLY.0) as u32;
        let mut got = 0u32;
        if filter.Init(flags, &[], &mut got) < 0 {
            return Err("the text extractor refused this file".into());
        }
        let mut out = String::new();
        let mut buf = vec![0u16; 8192];
        loop {
            let mut stat = STAT_CHUNK::default();
            let hr = filter.GetChunk(&mut stat);
            if hr == FILTER_E_END_OF_CHUNKS.0 {
                break;
            }
            if hr == FILTER_E_EMBEDDING_UNAVAILABLE.0 || hr == FILTER_E_LINK_UNAVAILABLE.0 {
                continue;
            }
            if hr < 0 {
                break;
            }
            if stat.flags != CHUNK_TEXT {
                continue;
            }
            if stat.breakType != CHUNK_NO_BREAK && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            loop {
                let mut n = buf.len() as u32;
                let hr = filter.GetText(&mut n, PWSTR(buf.as_mut_ptr()));
                if hr == FILTER_E_NO_MORE_TEXT.0 || hr < 0 {
                    break;
                }
                out.push_str(&String::from_utf16_lossy(&buf[..n as usize]));
                if hr == FILTER_S_LAST_TEXT.0 || out.len() > max {
                    break;
                }
            }
            if out.len() > max {
                break;
            }
        }
        Ok(out)
    }
}

/// RTF to plain paragraphs: control words dropped, `\par` breaks lines, `\'hh` and `\uN`
/// characters kept, and the font, colour, style and picture tables skipped.
pub fn rtf_text(raw: &str) -> Vec<(bool, String)> {
    let b = raw.as_bytes();
    let mut out = String::new();
    // Per group: whether its text is skipped, and how many characters follow a \uN.
    let mut stack: Vec<(bool, usize)> = vec![(false, 1)];
    let mut i = 0;
    let mut skip_chars = 0usize;
    while i < b.len() {
        let c = b[i];
        let (skip, uc) = *stack.last().unwrap();
        match c {
            b'{' => {
                stack.push((skip, uc));
                i += 1;
            }
            b'}' => {
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
            }
            b'\\' => {
                i += 1;
                if i >= b.len() {
                    break;
                }
                let n = b[i];
                if n == b'\'' && i + 2 < b.len() {
                    // \'hh: a byte in the document's code page (cp1252 for Western text).
                    let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                    i += 3;
                    if skip_chars > 0 {
                        skip_chars -= 1;
                    } else if !skip {
                        if let Some(v) = hex {
                            out.push(cp1252(v));
                        }
                    }
                    continue;
                }
                if !n.is_ascii_alphabetic() {
                    // \\ \{ \} and symbols (\~ a non-breaking space, \* a destination to skip).
                    if n == b'*' {
                        if let Some(top) = stack.last_mut() {
                            top.0 = true;
                        }
                    } else if !skip {
                        match n {
                            b'~' => out.push(' '),
                            b'\\' | b'{' | b'}' => out.push(n as char),
                            _ => {}
                        }
                    }
                    i += 1;
                    continue;
                }
                let start = i;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = &raw[start..i];
                let num_start = i;
                if i < b.len() && (b[i] == b'-' || b[i].is_ascii_digit()) {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let num: Option<i32> = raw[num_start..i].parse().ok();
                if i < b.len() && b[i] == b' ' {
                    i += 1;
                }
                match word {
                    "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "header" | "footer" | "headerl" | "headerr" | "footerl"
                    | "footerr" | "listtable" | "listoverridetable" | "rsidtbl" | "themedata" | "colorschememapping" | "latentstyles"
                    | "datastore" | "xmlnstbl" | "generator" | "object" | "fldinst" => {
                        if let Some(top) = stack.last_mut() {
                            top.0 = true;
                        }
                    }
                    "par" | "line" | "sect" | "page" | "row" if !skip => out.push('\n'),
                    "cell" | "tab" if !skip => out.push('\t'),
                    "uc" => {
                        if let Some(top) = stack.last_mut() {
                            top.1 = num.unwrap_or(1).max(0) as usize;
                        }
                    }
                    "u" if !skip => {
                        if let Some(v) = num {
                            let v = if v < 0 { v + 65536 } else { v } as u32;
                            if let Some(ch) = char::from_u32(v) {
                                out.push(ch);
                            }
                            skip_chars = uc;
                        }
                    }
                    _ => {}
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                if skip_chars > 0 {
                    skip_chars -= 1;
                } else if !skip {
                    out.push(c as char);
                }
                i += 1;
            }
        }
    }
    tidy_lines(&out)
}

/// Lines, with Symbol-font bullets (Word writes them as private-use characters) as real ones
/// and a list marker's tab as a space.
fn tidy_lines(text: &str) -> Vec<(bool, String)> {
    text.split('\n')
        .map(|l| {
            let l: String = l
                .chars()
                .map(|c| match c as u32 {
                    0xF0B7 | 0xB7 => '•',
                    0xF0A7 => '▪',
                    0xF0D8 => '➢',
                    0xF06F => 'o',
                    _ => c,
                })
                .collect();
            let l = l.trim();
            match l.split_once('\t') {
                Some((marker, rest)) if marker.chars().count() <= 4 => format!("{marker} {}", rest.trim()),
                _ => l.replace('\t', "  "),
            }
        })
        .filter(|l| !l.is_empty())
        .map(|l| (false, l))
        .collect()
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}',
        '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}',
        '\u{2013}', '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
    ];
    match b {
        0x80..=0x9f => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// A Word 97–2003 document's text, from its piece table (the CLX in the table stream).
pub fn doc97_text(path: &str) -> Result<Vec<(bool, String)>, String> {
    use std::io::Read;
    let mut comp = cfb::open(path).map_err(|e| e.to_string())?;
    let mut word = Vec::new();
    comp.open_stream("/WordDocument").map_err(|e| e.to_string())?.read_to_end(&mut word).map_err(|e| e.to_string())?;
    let u16_at = |b: &[u8], o: usize| -> Option<u16> { b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]])) };
    let u32_at = |b: &[u8], o: usize| -> Option<u32> { b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])) };
    if u16_at(&word, 0) != Some(0xA5EC) {
        return Err("not a Word 97 document".into());
    }
    let flags = u16_at(&word, 0x0A).unwrap_or(0);
    let table_name = if flags & 0x0200 != 0 { "/1Table" } else { "/0Table" };
    let mut table = Vec::new();
    comp.open_stream(table_name).map_err(|e| e.to_string())?.read_to_end(&mut table).map_err(|e| e.to_string())?;
    let fc_clx = u32_at(&word, 0x01A2).ok_or("short FIB")? as usize;
    let lcb_clx = u32_at(&word, 0x01A6).ok_or("short FIB")? as usize;
    let clx = table.get(fc_clx..fc_clx + lcb_clx).ok_or("bad CLX")?;
    // Skip the Prcs (0x01 + size + data), then the Pcdt (0x02 + size + PlcPcd).
    let mut o = 0;
    while clx.get(o) == Some(&1) {
        o += 3 + u16_at(clx, o + 1).unwrap_or(0) as usize;
    }
    if clx.get(o) != Some(&2) {
        return Err("no piece table".into());
    }
    let lcb = u32_at(clx, o + 1).ok_or("bad piece table")? as usize;
    let plc = clx.get(o + 5..o + 5 + lcb).ok_or("bad piece table")?;
    let n = lcb.saturating_sub(4) / 12;
    let mut text = String::new();
    for k in 0..n {
        let (cp0, cp1) = (u32_at(plc, k * 4).unwrap_or(0), u32_at(plc, (k + 1) * 4).unwrap_or(0));
        let count = cp1.saturating_sub(cp0) as usize;
        let pcd = (n + 1) * 4 + k * 8;
        let fc = u32_at(plc, pcd + 2).unwrap_or(0);
        if fc & 0x4000_0000 != 0 {
            let at = ((fc & 0x3FFF_FFFF) / 2) as usize;
            if let Some(bytes) = word.get(at..at + count) {
                text.extend(bytes.iter().map(|&b| cp1252(b)));
            }
        } else {
            let at = fc as usize;
            if let Some(bytes) = word.get(at..at + count * 2) {
                let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                text.push_str(&String::from_utf16_lossy(&units));
            }
        }
        if text.len() > 400_000 {
            break;
        }
    }
    Ok(clean_word_text(&text))
}

/// Word's special characters: \r ends a paragraph, 0x07 ends a cell, fields keep their
/// result (between 0x14 and 0x15) and drop their code (0x13 to 0x14).
fn clean_word_text(text: &str) -> Vec<(bool, String)> {
    let mut out = String::new();
    let mut depth: Vec<bool> = Vec::new();
    for c in text.chars() {
        match c {
            '\u{13}' => depth.push(true),
            '\u{14}' => {
                if let Some(d) = depth.last_mut() {
                    *d = false;
                }
            }
            '\u{15}' => {
                depth.pop();
            }
            _ if depth.last() == Some(&true) => {}
            '\r' | '\u{0b}' | '\u{0c}' => out.push('\n'),
            '\u{07}' => out.push('\t'),
            '\u{01}' | '\u{08}' | '\u{02}' | '\u{05}' => {}
            c => out.push(c),
        }
    }
    out.split('\n').map(|l| l.trim().trim_matches('\t').to_string()).filter(|l| !l.is_empty()).map(|l| (false, l)).collect()
}

/// A PowerPoint 97–2003 deck's text: its text atoms, slide by slide (masters and notes left out).
pub fn ppt97_text(path: &str) -> Result<Vec<(bool, String)>, String> {
    use std::io::Read;
    let mut comp = cfb::open(path).map_err(|e| e.to_string())?;
    let mut doc = Vec::new();
    comp.open_stream("/PowerPoint Document").map_err(|e| e.to_string())?.read_to_end(&mut doc).map_err(|e| e.to_string())?;
    fn push_lines(out: &mut Vec<(bool, String)>, s: &str) {
        let mut first = true;
        for l in s.split(['\r', '\u{0b}', '\n']).map(str::trim).filter(|l| !l.is_empty()) {
            // A text box's first line reads as its heading (slide titles come first).
            out.push((first && l.len() < 90, l.to_string()));
            first = false;
        }
    }
    fn walk(b: &[u8], out: &mut Vec<(bool, String)>, depth: usize) {
        let mut o = 0;
        while o + 8 <= b.len() && out.len() < 4000 {
            let ver = u16::from_le_bytes([b[o], b[o + 1]]) & 0x0F;
            let ty = u16::from_le_bytes([b[o + 2], b[o + 3]]);
            let len = u32::from_le_bytes([b[o + 4], b[o + 5], b[o + 6], b[o + 7]]) as usize;
            let body = &b[(o + 8).min(b.len())..(o + 8 + len).min(b.len())];
            match ty {
                // Main masters, notes and handouts hold placeholder text, not the deck's.
                0x03F8 | 0x03F0 | 0x0FF1 | 0x1004 => {}
                // TextCharsAtom (UTF-16) and TextBytesAtom (8-bit).
                0x0FA0 => {
                    let units: Vec<u16> = body.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    push_lines(out, &String::from_utf16_lossy(&units));
                }
                0x0FA8 => push_lines(out, &body.iter().map(|&c| cp1252(c)).collect::<String>()),
                _ if ver == 0x0F && depth < 12 => walk(body, out, depth + 1),
                _ => {}
            }
            o += 8 + len;
        }
    }
    let mut out = Vec::new();
    walk(&doc, &mut out, 0);
    // The same text often appears twice (the outline and the slide's own shapes).
    let mut seen = std::collections::HashSet::new();
    out.retain(|(_, t)| seen.insert(t.clone()));
    if out.is_empty() { Err("no text".into()) } else { Ok(out) }
}
