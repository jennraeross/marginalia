use epub::doc::{EpubDoc, NavPoint};
use scraper::{Html, Selector};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum Block {
    Heading(u8, String),
    Paragraph(String),
    Quote(String),
    ListItem(String),
    Pre(String),
    /// An inline illustration extracted from the EPUB. `hash` identifies the
    /// bytes for the GPU texture cache; `w`/`h` are the native dimensions in
    /// pixels (validated at parse time, so `image` can decode it).
    Image {
        bytes: Vec<u8>,
        alt: String,
        hash: u64,
        w: u32,
        h: u32,
    },
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub index: usize,
    pub label: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub struct TocEntry {
    pub depth: usize,
    pub label: String,
    pub chapter: Option<usize>,
}

pub fn chapter_count(path: &Path) -> Option<usize> {
    let doc = EpubDoc::new(path).ok()?;
    Some(doc.get_num_chapters())
}

pub fn load_book(path: &Path) -> Result<(Vec<TocEntry>, Vec<Chapter>), String> {
    let mut doc = EpubDoc::new(path).map_err(|e| format!("cannot open epub: {e}"))?;
    let n = doc.get_num_chapters();
    let mut chapters = Vec::with_capacity(n);
    for i in 0..n {
        if !doc.set_current_chapter(i) {
            continue;
        }
        let (html, _mime) = doc.get_current_str().unwrap_or_default();
        let ch_path = doc.get_current_path().unwrap_or_default();
        let mut resolve = |src: &str| resolve_resource(&mut doc, &ch_path, src);
        let blocks = html_to_blocks(&html, &mut resolve);
        chapters.push(Chapter {
            index: i,
            label: format!("Chapter {}", i + 1),
            blocks,
        });
    }
    // TOC: map nav content paths to spine chapters.
    let mut toc = Vec::new();
    flatten_toc(&doc.toc, 0, &doc, &mut toc);
    if toc.is_empty() {
        // Fallback: headings as TOC.
        for (i, ch) in chapters.iter().enumerate() {
            for b in &ch.blocks {
                if let Block::Heading(_, t) = b {
                    toc.push(TocEntry {
                        depth: 0,
                        label: t.clone(),
                        chapter: Some(i),
                    });
                    break;
                }
            }
        }
    }
    // Label chapters from TOC when possible. Blank TOC labels are ignored so
    // a chapter can never end up untitled.
    let mut mapped = vec![false; chapters.len()];
    for t in &toc {
        let Some(c) = t.chapter else { continue };
        let label = t.label.trim();
        if label.is_empty() {
            continue;
        }
        if let Some(ch) = chapters.get_mut(c) {
            if ch.label.starts_with("Chapter") {
                ch.label = label.to_string();
                mapped[c] = true;
            }
        }
    }
    // Chapters with no TOC entry (title pages, cover pages, split leftovers)
    // would otherwise keep a misleading positional "Chapter N" label. Prefer
    // their own first heading when they have one; the heading block is removed
    // so the reader doesn't show the title twice.
    for (i, ch) in chapters.iter_mut().enumerate() {
        if mapped[i] {
            continue;
        }
        let first_title = match ch.blocks.first() {
            Some(Block::Heading(_, t)) if !t.trim().is_empty() => Some(t.trim().to_string()),
            _ => None,
        };
        if let Some(title) = first_title {
            ch.label = title;
            ch.blocks.remove(0);
        }
    }
    Ok((toc, chapters))
}

fn flatten_toc(
    points: &[NavPoint],
    depth: usize,
    doc: &EpubDoc<std::io::BufReader<std::fs::File>>,
    out: &mut Vec<TocEntry>,
) {
    // EpubDoc generic over reader; use a helper via resource lookup done by caller path mapping.
    // We resolve chapter by trying resource_uri_to_chapter on content path.
    for p in points {
        let chapter = doc.resource_uri_to_chapter(&p.content);
        out.push(TocEntry {
            depth: depth.min(3),
            label: p.label.clone(),
            chapter,
        });
        if !p.children.is_empty() {
            flatten_toc_inner(&p.children, depth + 1, doc, out);
        }
    }
}

fn flatten_toc_inner(
    points: &[NavPoint],
    depth: usize,
    doc: &EpubDoc<std::io::BufReader<std::fs::File>>,
    out: &mut Vec<TocEntry>,
) {
    for p in points {
        let chapter = doc.resource_uri_to_chapter(&p.content);
        out.push(TocEntry {
            depth: depth.min(3),
            label: p.label.clone(),
            chapter,
        });
        if !p.children.is_empty() {
            flatten_toc_inner(&p.children, depth + 1, doc, out);
        }
    }
}

/// Load from a path with generic reader workaround: reopen for TOC mapping.
pub fn strip_html(html: &str) -> String {
    if html.is_empty() {
        return String::new();
    }
    let frag = Html::parse_fragment(html);
    let text = frag.root_element().text().collect::<Vec<_>>().join(" ");
    let unescaped = html_escape::decode_html_entities(&text).to_string();
    unescaped.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn text_of(el: scraper::ElementRef) -> String {
    let t = el.text().collect::<Vec<_>>().join(" ");
    html_escape::decode_html_entities(&t)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse chapter HTML into blocks. `lookup` resolves an `<img src>` value to
/// raw file bytes (the caller joins it against the chapter's location);
/// images that can't be resolved or decoded are skipped.
pub fn html_to_blocks<F>(html: &str, lookup: &mut F) -> Vec<Block>
where
    F: FnMut(&str) -> Option<Vec<u8>>,
{
    let doc = Html::parse_document(html);
    let Ok(body_sel) = Selector::parse("body") else {
        return vec![];
    };
    let body = doc.select(&body_sel).next();
    let root = match body {
        Some(b) => b.html(),
        None => html.to_string(),
    };
    let frag = Html::parse_fragment(&root);
    let mut blocks = Vec::new();
    collect_blocks(frag.root_element(), &mut blocks, lookup);
    // Merge consecutive whitespace-only away; image blocks always survive.
    blocks
        .into_iter()
        .filter(|b| matches!(b, Block::Image { .. }) || !block_text(b).is_empty())
        .collect()
}

fn block_text(b: &Block) -> &str {
    match b {
        Block::Heading(_, t)
        | Block::Paragraph(t)
        | Block::Quote(t)
        | Block::ListItem(t)
        | Block::Pre(t) => t,
        // Images carry no text (their alt text renders separately, if at all).
        Block::Image { .. } => "",
    }
}

/// Inline (non-block) elements: their text flows into the surrounding paragraph.
fn is_inline(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "abbr"
            | "acronym"
            | "b"
            | "bdi"
            | "bdo"
            | "big"
            | "cite"
            | "code"
            | "data"
            | "dfn"
            | "em"
            | "font"
            | "i"
            | "kbd"
            | "mark"
            | "nobr"
            | "q"
            | "rp"
            | "rt"
            | "ruby"
            | "s"
            | "samp"
            | "small"
            | "span"
            | "strike"
            | "strong"
            | "sub"
            | "sup"
            | "time"
            | "tt"
            | "u"
            | "var"
            | "wbr"
    )
}

fn squash_newlines(t: &str) -> String {
    t.chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect()
}

/// Text of an element for inline contexts: descendant text with `<br>` kept as
/// newlines so explicit line breaks survive (source newlines are just HTML
/// formatting noise and are collapsed to spaces).
fn inline_text(el: scraper::ElementRef) -> String {
    use scraper::node::Node;
    let mut out = String::new();
    for child in el.children() {
        match child.value() {
            Node::Text(t) => out.push_str(&squash_newlines(t)),
            Node::Element(e) if e.name() == "br" => out.push('\n'),
            Node::Element(_) => {
                if let Some(inner) = scraper::ElementRef::wrap(child) {
                    out.push_str(&inline_text(inner));
                }
            }
            _ => {}
        }
    }
    out
}

/// Flush accumulated inline text into paragraph blocks. Blank lines (from
/// `<br><br>` or block boundaries) start new paragraphs; single newlines
/// (from a lone `<br>`) are preserved as line breaks inside the block.
fn push_text_blocks(pending: &mut String, out: &mut Vec<Block>) {
    if pending.trim().is_empty() {
        pending.clear();
        return;
    }
    let decoded = html_escape::decode_html_entities(pending).into_owned();
    let mut current = String::new();
    let mut chunks: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if !current.trim().is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(&line);
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    for c in chunks {
        out.push(Block::Paragraph(c));
    }
    pending.clear();
}

fn collect_blocks<F>(parent: scraper::ElementRef, out: &mut Vec<Block>, lookup: &mut F)
where
    F: FnMut(&str) -> Option<Vec<u8>>,
{
    use scraper::node::Node;
    let mut pending = String::new();
    for child in parent.children() {
        match child.value() {
            Node::Text(t) => {
                pending.push_str(&squash_newlines(t));
            }
            Node::Element(e) if e.name() == "br" => {
                pending.push('\n');
            }
            Node::Element(e) if e.name() == "img" => {
                // Illustrations are block-level, but only split the paragraph
                // when the image actually materializes; broken references
                // vanish without disturbing the text flow.
                if let Some(inner) = scraper::ElementRef::wrap(child) {
                    let src = inner.value().attr("src").unwrap_or("");
                    let alt = inner.value().attr("alt").unwrap_or("");
                    if let Some(block) = image_block(lookup, src, alt) {
                        push_text_blocks(&mut pending, out);
                        out.push(block);
                    }
                }
            }
            // Vector and SVG-embedded raster content isn't rasterized;
            // text on either side still joins.
            Node::Element(e) if e.name() == "image" || e.name() == "svg" => {}
            Node::Element(e) if is_inline(e.name()) => {
                if let Some(inner) = scraper::ElementRef::wrap(child) {
                    pending.push_str(&inline_text(inner));
                }
            }
            Node::Element(_) => {
                let Some(el) = scraper::ElementRef::wrap(child) else {
                    continue;
                };
                // Block boundary: flush first so paragraphs never merge.
                push_text_blocks(&mut pending, out);
                collect_block_element(el, out, lookup);
            }
            _ => {}
        }
    }
    push_text_blocks(&mut pending, out);
    // Fallback: if nothing collected, use all text as paragraphs.
    if out.is_empty() {
        let t = text_of(parent);
        for para in t.split("\n\n") {
            let p = para.split_whitespace().collect::<Vec<_>>().join(" ");
            if !p.is_empty() {
                out.push(Block::Paragraph(p));
            }
        }
    }
}

/// One block-level element. Containers recurse so nested `<div>` soup can
/// never flatten a chapter into a single run-on paragraph.
fn collect_block_element<F>(el: scraper::ElementRef, out: &mut Vec<Block>, lookup: &mut F)
where
    F: FnMut(&str) -> Option<Vec<u8>>,
{
    let tag = el.value().name();
    match tag {
        "h1" => out.push(Block::Heading(1, text_of(el))),
        "h2" => out.push(Block::Heading(2, text_of(el))),
        "h3" => out.push(Block::Heading(3, text_of(el))),
        "h4" | "h5" | "h6" => out.push(Block::Heading(4, text_of(el))),
        "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "aside" | "nav"
        | "address" | "figure" | "figcaption" | "details" | "summary" | "table" | "thead"
        | "tbody" | "tfoot" | "tr" | "td" | "th" | "dl" | "dt" | "dd" | "form" => {
            collect_blocks(el, out, lookup);
        }
        "blockquote" => {
            let mut inner = Vec::new();
            collect_blocks(el, &mut inner, lookup);
            for b in inner {
                match b {
                    Block::Paragraph(t) => out.push(Block::Quote(t)),
                    other => out.push(other),
                }
            }
        }
        "pre" => out.push(Block::Pre(el.inner_html())),
        "ul" | "ol" => {
            // Direct items only: nested lists render as their own items.
            let mut any = false;
            for child in el.children() {
                let Some(li) = scraper::ElementRef::wrap(child) else {
                    continue;
                };
                if li.value().name() != "li" {
                    continue;
                }
                let t = text_of(li);
                if !t.is_empty() {
                    out.push(Block::ListItem(t));
                    any = true;
                }
            }
            if !any {
                // Malformed list without direct items: recurse instead of dropping.
                collect_blocks(el, out, lookup);
            }
        }
        "li" => {
            let t = text_of(el);
            if !t.is_empty() {
                out.push(Block::ListItem(t));
            }
        }
        "hr" => {
            // Thematic break: boundary only, no text of its own.
        }
        _ => {
            // Unknown element: container with children recurses, otherwise its
            // text joins the surrounding paragraph.
            let has_el_children = el
                .children()
                .any(|c| scraper::ElementRef::wrap(c).is_some());
            if has_el_children {
                collect_blocks(el, out, lookup);
            } else {
                let mut pending = inline_text(el);
                push_text_blocks(&mut pending, out);
            }
        }
    }
}

/// Build an image block from an `<img>` `src`, or `None` when the bytes can't
/// be resolved or decoded (remote URLs, SVG, corrupt files, ...).
pub(crate) fn image_block<F>(lookup: &mut F, src: &str, alt: &str) -> Option<Block>
where
    F: FnMut(&str) -> Option<Vec<u8>>,
{
    if src.trim().is_empty() {
        return None;
    }
    let bytes = lookup(src)?;
    // Refuse absurdly large files before decoding.
    if bytes.len() > 20_000_000 {
        return None;
    }
    // Header-only sniff: validates the format and gives dimensions without a
    // full decode (unsupported types like SVG fail here and are skipped).
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&bytes, &mut hasher);
    Some(Block::Image {
        bytes,
        alt: alt.to_string(),
        hash: std::hash::Hasher::finish(&hasher),
        w,
        h,
    })
}

/// Canonical forward-slash form of an archive path, for comparing against
/// resource entries regardless of platform.
pub(crate) fn canon_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Join an `<img src>` onto its chapter directory, resolving `.`/`..`
/// lexically and stripping any query string or fragment. A leading `/`
/// counts from the archive root. Returns forward-slash archive paths.
pub(crate) fn normalize_join(base: &Path, src: &str) -> String {
    let mut parts: Vec<String> = canon_path(base)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    // Absolute `src` (rare) counts from the archive root.
    let mut rel = src.trim();
    if let Some(rest) = rel.strip_prefix('/') {
        parts.clear();
        rel = rest;
    }
    for comp in rel.split(['?', '#']).next().unwrap_or("").split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c.to_string()),
        }
    }
    parts.join("/")
}

/// ASCII-only `%XX` decoding for percent-encoded resource paths.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // `i + 2` must be a valid index (saturating for overflow safety).
        if bytes[i] == b'%'
            && i.saturating_add(2) < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = &s[i + 1..i + 3];
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve an `<img src>` (relative to `chapter_path`) to file bytes.
/// Tries the literal location first, then a percent-decoded variant, then a
/// unique file-name match among the manifest resources (some producers use
/// inconsistent path forms). Returns `None` for remote URLs.
pub(crate) fn resolve_resource<R: std::io::Read + std::io::Seek>(
    doc: &mut EpubDoc<R>,
    chapter_path: &Path,
    src: &str,
) -> Option<Vec<u8>> {
    let src = src.trim();
    if src.is_empty() {
        return None;
    }
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("data:")
        || lower.starts_with("blob:")
    {
        return None;
    }
    let base = chapter_path.parent().unwrap_or_else(|| Path::new(""));
    for candidate in [
        normalize_join(base, src),
        normalize_join(base, &percent_decode(src)),
    ] {
        if candidate.is_empty() {
            continue;
        }
        let hit = doc
            .resources
            .values()
            .find(|r| canon_path(&r.path) == candidate)
            .map(|r| r.path.clone())
            .or_else(|| {
                // Fallback: a unique file-name match.
                let fname = candidate.rsplit('/').next().unwrap_or("");
                if fname.is_empty() {
                    return None;
                }
                let mut it = doc.resources.values().filter(|r| {
                    r.path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy() == fname)
                });
                let first = it.next()?.path.clone();
                if it.next().is_some() {
                    return None; // ambiguous: refuse to guess wrong
                }
                Some(first)
            });
        if let Some(path) = hit {
            // Try both the stored path and its forward-slash form, since zip
            // lookup semantics vary by platform.
            if let Some(data) = doc.get_resource_by_path(&path) {
                return Some(data);
            }
            let fwd = canon_path(&path);
            if let Some(data) = doc.get_resource_by_path(fwd) {
                return Some(data);
            }
        }
    }
    None
}

pub fn cover_bytes(library_root: &Path, book: &crate::library::Book) -> Option<(Vec<u8>, PathBuf)> {
    let epub_path = library_root.join(&book.file_path);
    let dir = epub_path.parent()?;
    for name in ["cover.jpg", "cover.png", "cover.gif", "cover.webp"] {
        let p = dir.join(name);
        if p.exists() {
            if let Ok(b) = std::fs::read(&p) {
                return Some((b, p));
            }
        }
    }
    None
}

/// Estimated readable characters in a block (headings weigh slightly more).
pub fn block_chars(b: &Block) -> usize {
    // Images count as a paragraph-ish chunk for layout heuristics.
    if matches!(b, Block::Image { .. }) {
        return 400;
    }
    let t = block_text(b);
    let n = t.chars().count();
    match b {
        Block::Heading(_, _) => n + 40,
        Block::Pre(_) => n / 2,
        _ => n,
    }
}

pub fn total_chars(blocks: &[Block]) -> usize {
    blocks.iter().map(block_chars).sum()
}

/// Split `text` into chunks of at most `max_chars` characters, preferring to cut
/// at whitespace. Always makes progress (never returns empty or stuck chunks).
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + max_chars).min(chars.len());
        if end < chars.len() {
            // Prefer a whitespace boundary so words stay intact.
            let mut cut = end;
            while cut > start && !chars[cut].is_whitespace() && end - cut < max_chars / 2 {
                cut -= 1;
            }
            if cut > start && chars[cut].is_whitespace() {
                end = cut;
            }
        }
        let s: String = chars[start..end].iter().collect();
        if s.trim().is_empty() {
            // Degenerate (long whitespace run): skip ahead instead of stalling.
            start = end.max(start + 1);
            continue;
        }
        chunks.push(s);
        start = end;
        // Skip the whitespace we cut on so the next chunk starts clean.
        while start < chars.len() && chars[start].is_whitespace() {
            start += 1;
        }
    }
    if chunks.is_empty() {
        chunks.push(text.trim().to_string());
    }
    chunks
}

/// Whether `Auto` column mode should use two columns for this width.
/// Always two columns unless the viewport is too narrow for two comfortable
/// reading columns (two minimum-width columns plus the inter-column gap).
pub fn auto_two_columns(content_w: f32, _total: usize) -> bool {
    const MIN_COLUMN_WIDTH: f32 = 320.0;
    const COLUMN_GAP: f32 = 48.0;
    content_w >= 2.0 * MIN_COLUMN_WIDTH + COLUMN_GAP
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(blocks: &[Block]) -> Vec<&str> {
        blocks
            .iter()
            .map(|b| match b {
                Block::Heading(_, t)
                | Block::Paragraph(t)
                | Block::Quote(t)
                | Block::ListItem(t)
                | Block::Pre(t) => t.as_str(),
                Block::Image { alt, .. } => alt.as_str(),
            })
            .collect()
    }

    #[test]
    fn auto_thresholds() {
        // Two minimum-width columns (320pt each) plus the 48pt gap = 688pt.
        assert!(auto_two_columns(760.0, 5000));
        assert!(auto_two_columns(760.0, 100));
        assert!(auto_two_columns(700.0, 0));
        assert!(!auto_two_columns(600.0, 5000));
        assert!(!auto_two_columns(500.0, 0));
    }

    #[test]
    fn chunk_prefers_word_boundaries() {
        let chunks = chunk_text("aaa bbb ccc ddd", 7);
        assert!(chunks.iter().all(|c| c.chars().count() <= 7));
        assert_eq!(chunks.concat().replace(' ', ""), "aaabbbcccddd");
        // Hard cut when a single word exceeds the limit.
        let chunks = chunk_text("abcdefghij", 4);
        assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn div_soup_stays_separate() {
        // WordPress/SB-style markup: nested divs must not flatten into one block.
        let blocks = html_to_blocks(
            "<div><div>First.</div><div>Second.</div></div>",
            &mut |_: &str| None,
        );
        assert_eq!(texts(&blocks), vec!["First.", "Second."]);
    }

    #[test]
    fn plain_paragraphs_stay_separate() {
        let blocks = html_to_blocks("<p>One.</p><p>Two.</p><p>Three.</p>", &mut |_: &str| None);
        assert_eq!(texts(&blocks), vec!["One.", "Two.", "Three."]);
    }

    #[test]
    fn double_br_splits_paragraphs() {
        let blocks = html_to_blocks("<p>line one<br><br>line two</p>", &mut |_: &str| None);
        assert_eq!(texts(&blocks), vec!["line one", "line two"]);
    }

    #[test]
    fn single_br_is_a_line_break() {
        let blocks = html_to_blocks("<p>aaa<br>bbb</p>", &mut |_: &str| None);
        assert_eq!(blocks.len(), 1);
        assert!(texts(&blocks)[0].contains('\n'));
    }

    #[test]
    fn mixed_content_keeps_order() {
        let blocks = html_to_blocks("<div>Intro<p>Body.</p>Tail.</div>", &mut |_: &str| None);
        assert_eq!(texts(&blocks), vec!["Intro", "Body.", "Tail."]);
    }

    #[test]
    fn inline_markup_stays_in_flow() {
        let blocks = html_to_blocks("<p>foo <b>bar</b> baz</p>", &mut |_: &str| None);
        assert_eq!(texts(&blocks), vec!["foo bar baz"]);
    }

    /// Tiny valid PNG bytes for image tests (solid color, no files needed).
    fn tiny_png(w: u32, h: u32) -> Vec<u8> {
        use image::codecs::png::PngEncoder;
        use image::ImageEncoder;
        let pixels = vec![200u8; (w * h * 4) as usize];
        let mut buf = Vec::new();
        PngEncoder::new(&mut buf)
            .write_image(&pixels, w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        buf
    }

    fn image_lookup(bytes: &[u8]) -> impl FnMut(&str) -> Option<Vec<u8>> + '_ {
        move |src: &str| {
            if src == "pic.png" {
                Some(bytes.to_vec())
            } else {
                None
            }
        }
    }

    #[test]
    fn img_block_extracted_with_dims() {
        let png = tiny_png(100, 50);
        let mut lookup = image_lookup(&png);
        let blocks = html_to_blocks(
            "<p>Before.</p><img src=\"pic.png\" alt=\"A pic\"/><p>After.</p>",
            &mut lookup,
        );
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], Block::Paragraph(_)));
        match &blocks[1] {
            Block::Image {
                alt, w, h, bytes, ..
            } => {
                assert_eq!(alt, "A pic");
                assert_eq!((*w, *h), (100, 50));
                assert_eq!(bytes, &png);
            }
            other => panic!("expected image, got {other:?}"),
        }
        assert!(matches!(&blocks[2], Block::Paragraph(_)));
        // Same bytes hash identically (stable texture cache keys).
        let mut lookup = image_lookup(&png);
        let again = html_to_blocks("<img src=\"pic.png\"/>", &mut lookup);
        let (h1, h2) = match (&blocks[1], &again[0]) {
            (Block::Image { hash: a, .. }, Block::Image { hash: b, .. }) => (*a, *b),
            _ => panic!("expected images"),
        };
        assert_eq!(h1, h2);
    }

    #[test]
    fn img_block_missing_src_keeps_text_flow() {
        // Unresolvable/broken images vanish without disturbing the text flow.
        // (Spacing follows HTML rules: only source whitespace creates gaps.)
        let mut lookup = image_lookup(&[]);
        let blocks = html_to_blocks(
            "<div>Before. <img src=\"gone.png\"/> After.</div>",
            &mut lookup,
        );
        assert_eq!(texts(&blocks), vec!["Before. After."]);
    }

    #[test]
    fn img_block_skips_remote_data_and_svg() {
        for src in [
            "http://example.com/a.png",
            "https://example.com/a.png",
            "data:image/png;base64,iVBOR",
            "pic.svg",
        ] {
            let png = tiny_png(10, 10);
            let mut lookup = image_lookup(&png);
            let html = format!("<p>Text.</p><img src=\"{src}\"/>");
            let blocks = html_to_blocks(&html, &mut lookup);
            assert_eq!(blocks.len(), 1, "src={src}");
            assert!(matches!(&blocks[0], Block::Paragraph(_)));
        }
        // Corrupt bytes are not images either.
        let mut lookup = |_: &str| Some(b"definitely not an image".to_vec());
        let blocks = html_to_blocks("<img src=\"x.png\"/>", &mut lookup);
        assert!(blocks.is_empty());
    }

    #[test]
    fn normalize_join_cases() {
        use std::path::Path;
        assert_eq!(
            normalize_join(Path::new("OEBPS/Text"), "img/pic.png"),
            "OEBPS/Text/img/pic.png"
        );
        assert_eq!(
            normalize_join(Path::new("OEBPS/Text"), "../Images/pic.png"),
            "OEBPS/Images/pic.png"
        );
        assert_eq!(normalize_join(Path::new("OEBPS"), "./a.png"), "OEBPS/a.png");
        assert_eq!(normalize_join(Path::new("OEBPS"), "/a.png"), "a.png");
        assert_eq!(normalize_join(Path::new(""), "a.png"), "a.png");
        assert_eq!(
            normalize_join(Path::new("OEBPS/Text"), "pic.png?v=1#frag"),
            "OEBPS/Text/pic.png"
        );
    }

    #[test]
    fn percent_decode_cases() {
        assert_eq!(percent_decode("plain.png"), "plain.png");
        assert_eq!(percent_decode("my%20pic.png"), "my pic.png");
        assert_eq!(percent_decode("%41%42"), "AB");
        assert_eq!(percent_decode("100%.png"), "100%.png");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
    }
}
