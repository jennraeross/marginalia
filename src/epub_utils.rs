use crate::epub_core::{EpubDoc, NavPoint};
use scraper::{Html, Selector};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub underline: bool,
    pub mono: bool,
}

impl Span {
    pub fn plain<S: Into<String>>(text: S) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }
}

/// One plain run, unless empty.
pub fn plain_spans(s: &str) -> Vec<Span> {
    if s.is_empty() {
        Vec::new()
    } else {
        vec![Span::plain(s.to_string())]
    }
}

/// Concatenated text of spans (layout/clipboard/speech order).
pub fn spans_plain(spans: &[Span]) -> String {
    let mut out = String::new();
    for s in spans {
        out.push_str(&s.text);
    }
    out
}

/// Total characters across spans (pagination heuristics use this).
pub fn spans_len(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.text.chars().count()).sum()
}

#[derive(Debug, Clone)]
pub enum Block {
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    Quote(Vec<Span>),
    ListItem(Vec<Span>),
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
    /// Stable spine href (forward-slash archive path) for annotation anchoring
    /// across re-imports. Falls back to `spine{i}` when unknown.
    pub href: String,
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

/// Spine hrefs + TOC without parsing chapter HTML. Cheap enough to run on the
/// UI thread: no chapter bodies are touched, so opening a book only pays for
/// the zip/opf/nav walk. The returned hrefs are positional (index `i` is
/// spine `i`); a chapter that can't be selected gets a `spine{i}` placeholder.
pub fn book_skeleton(path: &Path) -> Result<(Vec<TocEntry>, Vec<String>), String> {
    let mut doc = EpubDoc::new(path).map_err(|e| format!("cannot open epub: {e}"))?;
    let n = doc.get_num_chapters();
    let mut hrefs = Vec::with_capacity(n);
    for i in 0..n {
        if doc.set_current_chapter(i) {
            let ch_path = doc.get_current_path().unwrap_or_default();
            hrefs.push(canon_path(&ch_path));
        } else {
            hrefs.push(format!("spine{i}"));
        }
    }
    let mut toc = Vec::new();
    flatten_toc(&doc.toc, 0, &doc, &mut toc);
    Ok((toc, hrefs))
}

/// First non-blank TOC label pointing at spine `idx`, if any. Mirrors the
/// first-wins rule `load_book` applies when labelling chapters.
pub fn toc_label_for(toc: &[TocEntry], idx: usize) -> Option<String> {
    toc.iter().find_map(|t| {
        if t.chapter == Some(idx) {
            let label = t.label.trim();
            if !label.is_empty() {
                return Some(label.to_string());
            }
        }
        None
    })
}

/// Parse a single spine chapter. Labelling mirrors `load_book`: the nav label
/// wins when present; chapters with no nav entry fall back to their first
/// heading (stripped, as in `load_book`) or `Chapter N`. When the book has no
/// nav TOC at all, headings are kept: `load_book` would have mapped them via
/// its heading fallback, and the reader removes the duplicate title at
/// pagination time.
pub fn load_chapter(path: &Path, idx: usize, toc: &[TocEntry]) -> Result<Chapter, String> {
    let mut doc = EpubDoc::new(path).map_err(|e| format!("cannot open epub: {e}"))?;
    parse_chapter(&mut doc, idx, toc)
}

fn parse_chapter(doc: &mut EpubDoc, idx: usize, toc: &[TocEntry]) -> Result<Chapter, String> {
    if !doc.set_current_chapter(idx) {
        return Err(format!("no chapter {idx}"));
    }
    let (html, _mime) = doc.get_current_str().unwrap_or_default();
    let ch_path = doc.get_current_path().unwrap_or_default();
    let mut resolve = |src: &str| resolve_resource(doc, &ch_path, src);
    let mut blocks = html_to_blocks(&html, &mut resolve);
    let href = canon_path(&ch_path);
    let clean = toc_label_for(toc, idx);
    let mapped = clean.is_some();
    let mut label = clean.unwrap_or_default();
    if !mapped {
        label = format!("Chapter {}", idx + 1);
        if let Some(Block::Heading(_, spans)) = blocks.first() {
            if !spans_plain(spans).trim().is_empty() {
                label = spans_plain(spans).trim().to_string();
                // No nav TOC: the heading fallback would have mapped this
                // chapter, so keep its heading like `load_book` does (the
                // reader drops the duplicate title at pagination time).
                // With a nav TOC, unmapped chapters lose the heading.
                if !toc.is_empty() {
                    blocks.remove(0);
                }
            }
        }
    }
    Ok(Chapter {
        index: idx,
        label,
        href,
        blocks,
    })
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
            href: canon_path(&ch_path),
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
                if let Block::Heading(_, spans) = b {
                    toc.push(TocEntry {
                        depth: 0,
                        label: spans_plain(spans),
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
            Some(Block::Heading(_, spans)) if !spans_plain(spans).trim().is_empty() => {
                Some(spans_plain(spans).trim().to_string())
            }
            _ => None,
        };
        if let Some(title) = first_title {
            ch.label = title;
            ch.blocks.remove(0);
        }
    }
    Ok((toc, chapters))
}

fn flatten_toc(points: &[NavPoint], depth: usize, doc: &EpubDoc, out: &mut Vec<TocEntry>) {
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

fn flatten_toc_inner(points: &[NavPoint], depth: usize, doc: &EpubDoc, out: &mut Vec<TocEntry>) {
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

pub fn block_plain(b: &Block) -> String {
    match b {
        Block::Heading(_, s) | Block::Paragraph(s) | Block::Quote(s) | Block::ListItem(s) => {
            spans_plain(s)
        }
        Block::Pre(t) => t.clone(),
        // Images carry no text (their alt text renders separately, if at all).
        Block::Image { .. } => String::new(),
    }
}

/// Anchor text for highlights: what hit-testing and painting measure.
/// List items include the rendered bullet prefix so offsets match the reader.
pub fn anchor_text(b: &Block) -> String {
    match b {
        Block::ListItem(spans) => format!("• {}", spans_plain(spans)),
        Block::Heading(_, spans) | Block::Paragraph(spans) | Block::Quote(spans) => {
            spans_plain(spans)
        }
        Block::Pre(t) => t.clone(),
        Block::Image { .. } => String::new(),
    }
}

/// Char-index slice of `s`, clamped. Never panics on multi-byte boundaries.
pub fn slice_chars(s: &str, start: usize, end: usize) -> String {
    let (lo, hi) = (start.min(end), start.max(end));
    s.chars().skip(lo).take(hi.saturating_sub(lo)).collect()
}

fn clamp_range(len: usize, start: usize, end: usize) -> (usize, usize) {
    let (mut s, mut e) = (start.min(end), start.max(end));
    s = s.min(len);
    e = e.min(len);
    (s, e)
}

/// Display text of an anchor range: trimmed non-empty pieces joined with a
/// blank line, mirroring the reader's copy behavior.
pub fn anchor_range_text(texts: &[String], sb: usize, so: usize, eb: usize, eo: usize) -> String {
    if texts.is_empty() {
        return String::new();
    }
    let (sb, eb) = (sb.min(eb.max(sb)), eb);
    let last = eb.min(texts.len().saturating_sub(1));
    let mut parts = Vec::new();
    for (idx, t) in texts
        .iter()
        .enumerate()
        .skip(sb)
        .take(last.saturating_sub(sb) + 1)
    {
        let len = t.chars().count();
        let (s, e) = if sb == eb {
            clamp_range(len, so, eo)
        } else if idx == sb {
            (so.min(len), len)
        } else if idx == eb {
            (0, eo.min(len))
        } else {
            (0, len)
        };
        let piece = slice_chars(t, s, e);
        if !piece.trim().is_empty() {
            parts.push(piece.trim().to_string());
        }
    }
    parts.join("\n\n")
}

/// Context overlap score: chars of `prefix` matching backwards plus chars of
/// `suffix` matching forwards (max 32+32).
fn context_score(text: &str, pos: usize, needle_len: usize, prefix: &str, suffix: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut score = 0;
    let pre: Vec<char> = prefix.chars().collect();
    let suf: Vec<char> = suffix.chars().collect();
    let take_pre = pre.len().min(32).min(pos);
    for i in 0..take_pre {
        if chars[pos - 1 - i] == pre[pre.len() - 1 - i] {
            score += 1;
        } else {
            break;
        }
    }
    let after = pos + needle_len;
    let take_suf = suf.len().min(32).min(chars.len().saturating_sub(after));
    for i in 0..take_suf {
        if chars[after + i] == suf[i] {
            score += 1;
        } else {
            break;
        }
    }
    score
}

fn best_in_text(text: &str, needle: &str, prefix: &str, suffix: &str) -> Option<(usize, usize)> {
    if needle.is_empty() || text.is_empty() {
        return None;
    }
    let t: Vec<char> = text.chars().collect();
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() || t.len() < n.len() {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i + n.len() <= t.len() {
        if t[i..i + n.len()] == n[..] {
            let s = context_score(text, i, n.len(), prefix, suffix);
            let better = match best {
                None => true,
                Some((_, bs)) => s > bs,
            };
            if better {
                best = Some((i, s));
                if s == 64 {
                    break;
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    best.map(|(pos, _)| (pos, pos + n.len()))
}

/// Relocate an annotation range inside `texts` (anchor texts of one chapter).
/// Exact stored offsets win when they reproduce `quote`; otherwise fuzzy
/// quote search with prefix/suffix disambiguation. Returns layout offsets.
#[allow(clippy::too_many_arguments)]
pub fn find_anchor(
    texts: &[String],
    start_block: usize,
    start_offset: usize,
    end_block: usize,
    end_offset: usize,
    quote: &str,
    prefix: &str,
    suffix: &str,
) -> Option<(usize, usize, usize, usize)> {
    if texts.is_empty() || quote.trim().is_empty() {
        return None;
    }
    // Exact fast path.
    if start_block < texts.len() && end_block < texts.len() && start_block <= end_block {
        let reproduced = anchor_range_text(texts, start_block, start_offset, end_block, end_offset);
        if reproduced == quote {
            let sb_len = texts[start_block].chars().count();
            let eb_len = texts[end_block].chars().count();
            let (ss, _) = clamp_range(sb_len, start_offset, start_offset);
            let (_, ee) = clamp_range(eb_len, end_offset, end_offset);
            let (s, e) = (ss.min(sb_len), ee.min(eb_len));
            if start_block == end_block && s >= e {
                // Empty after clamping: fall through to fuzzy.
            } else {
                return Some((start_block, s, end_block, e));
            }
        }
    }
    let pieces: Vec<&str> = quote.split("\n\n").collect();
    if pieces.len() == 1 {
        let piece = pieces[0];
        let needles = if piece.chars().count() > 64 {
            // Long quote: try full, then a distinctive middle slice.
            let chars: Vec<char> = piece.chars().collect();
            let mid = chars.len() / 2;
            let lo = mid.saturating_sub(16);
            vec![
                piece.to_string(),
                chars[lo..(lo + 32).min(chars.len())].iter().collect(),
            ]
        } else {
            vec![piece.to_string()]
        };
        for needle in needles {
            let mut best: Option<(usize, usize, usize, usize, usize)> = None;
            for (bi, t) in texts.iter().enumerate() {
                if let Some((s, e)) = best_in_text(t, &needle, prefix, suffix) {
                    let sc = context_score(t, s, needle.chars().count(), prefix, suffix);
                    let better = match best {
                        None => true,
                        Some((_, _, _, _, bs)) => sc > bs,
                    };
                    if better {
                        best = Some((bi, s, bi, e, sc));
                    }
                }
            }
            if let Some((sb, so, eb, eo, _)) = best {
                return Some((sb, so, eb, eo));
            }
        }
        return None;
    }
    // Multi-block: anchor on first and last pieces.
    let first = pieces[0];
    let last = pieces[pieces.len() - 1];
    let first_needle: String = first.chars().take(64).collect();
    let last_needle: String = {
        let c: Vec<char> = last.chars().collect();
        c[c.len().saturating_sub(64)..].iter().collect()
    };
    let mut best: Option<(usize, usize, usize, usize, usize)> = None;
    for (sbi, st) in texts.iter().enumerate() {
        let Some((ss, _)) = best_in_text(st, &first_needle, prefix, "") else {
            continue;
        };
        for (ebi, et) in texts.iter().enumerate().skip(sbi) {
            let search_suffix = if ebi == sbi { "" } else { suffix };
            if let Some((_, ee)) = best_in_text(et, &last_needle, "", search_suffix) {
                // End offset is end of the matched needle.
                let sc = context_score(st, ss, first_needle.chars().count(), prefix, "")
                    + context_score(
                        et,
                        ee - last_needle.chars().count(),
                        last_needle.chars().count(),
                        "",
                        suffix,
                    );
                let better = match best {
                    None => true,
                    Some((_, _, _, _, bs)) => sc > bs,
                };
                if better {
                    // Start at match start; end at match end. For the common
                    // case where the piece equals the block slice, this is exact.
                    let (_, send) = best_in_text(et, &last_needle, "", search_suffix).unwrap();
                    best = Some((sbi, ss, ebi, send, sc));
                }
                break;
            }
        }
    }
    best.map(|(sb, so, eb, eo, _)| (sb, so, eb, eo))
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
        .filter(|b| matches!(b, Block::Image { .. }) || !block_plain(b).trim().is_empty())
        .collect()
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
            | "del"
            | "dfn"
            | "em"
            | "font"
            | "i"
            | "ins"
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

/// Accumulated inline style while walking descendants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    mono: bool,
}

fn apply_tag(tag: &str, mut s: Style) -> Style {
    match tag {
        "b" | "strong" => s.bold = true,
        "i" | "em" | "cite" | "dfn" | "var" => s.italic = true,
        "s" | "strike" | "del" => s.strike = true,
        "u" | "ins" | "a" => s.underline = true,
        "code" | "kbd" | "tt" | "samp" => s.mono = true,
        _ => {}
    }
    s
}

fn style_of_span(s: &Span) -> Style {
    Style {
        bold: s.bold,
        italic: s.italic,
        strike: s.strike,
        underline: s.underline,
        mono: s.mono,
    }
}

fn push_span(out: &mut Vec<Span>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    let decoded = html_escape::decode_html_entities(&text).into_owned();
    if decoded.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if style_of_span(last) == style {
            last.text.push_str(&decoded);
            return;
        }
    }
    out.push(Span {
        text: decoded,
        bold: style.bold,
        italic: style.italic,
        strike: style.strike,
        underline: style.underline,
        mono: style.mono,
    });
}

/// Recurse into `el`, applying its own tag on top of `base`.
fn collect_element_spans(el: scraper::ElementRef, base: Style, out: &mut Vec<Span>) {
    use scraper::node::Node;
    let style = apply_tag(el.value().name(), base);
    for child in el.children() {
        match child.value() {
            Node::Text(t) => push_span(out, squash_newlines(t), style),
            Node::Element(e) if e.name() == "br" => {
                out.push(Span {
                    text: "\n".to_string(),
                    ..Default::default()
                });
            }
            Node::Element(e) if e.name() == "img" || e.name() == "image" || e.name() == "svg" => {}
            Node::Element(_) => {
                if let Some(inner) = scraper::ElementRef::wrap(child) {
                    collect_element_spans(inner, style, out);
                }
            }
            _ => {}
        }
    }
}

/// All descendant text of `el` with `<br>` kept as newlines and inline tags
/// mapped to span styles. Images contribute nothing.
fn collect_inline_spans(el: scraper::ElementRef) -> Vec<Span> {
    use scraper::node::Node;
    let mut out = Vec::new();
    for child in el.children() {
        match child.value() {
            Node::Text(t) => push_span(&mut out, squash_newlines(t), Style::default()),
            Node::Element(e) if e.name() == "br" => {
                out.push(Span {
                    text: "\n".to_string(),
                    ..Default::default()
                });
            }
            Node::Element(e) if e.name() == "img" || e.name() == "image" || e.name() == "svg" => {}
            Node::Element(_) => {
                if let Some(inner) = scraper::ElementRef::wrap(child) {
                    collect_element_spans(inner, Style::default(), &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

/// Collapse one line's chars: trim ends, single-space inner runs. Spaces take
/// the preceding style so mono/bold widths stay consistent.
fn collapse_line(chars: &[(char, Style)]) -> Vec<(char, Style)> {
    let mut out = Vec::new();
    let mut pending_space: Option<Style> = None;
    let mut started = false;
    for (c, s) in chars {
        if c.is_whitespace() {
            if started && pending_space.is_none() {
                pending_space = Some(*s);
            }
        } else {
            if let Some(sp) = pending_space.take() {
                // Space inherits the previous non-space style when possible.
                let use_style = out.last().map(|(_, ps): &(_, Style)| *ps).unwrap_or(sp);
                out.push((' ', use_style));
            }
            out.push((*c, *s));
            started = true;
        }
    }
    out
}

/// Split spans on explicit newlines (`<br>`), collapse whitespace per line,
/// and regroup blank-line-separated lines into paragraphs. Single newlines
/// survive inside a paragraph; blank lines start a new one.
fn spans_to_paragraphs(spans: Vec<Span>) -> Vec<Vec<Span>> {
    let mut chars: Vec<(char, Style)> = Vec::new();
    for s in &spans {
        let st = style_of_span(s);
        for c in s.text.chars() {
            chars.push((c, st));
        }
    }
    // Split on '\n' (from <br>); other newlines were already squashed.
    let mut lines: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    for (c, s) in chars {
        if c == '\n' {
            lines.push(Vec::new());
        } else {
            lines.last_mut().unwrap().push((c, s));
        }
    }
    let mut paras: Vec<Vec<(char, Style)>> = Vec::new();
    let mut cur: Vec<(char, Style)> = Vec::new();
    for line in lines {
        let collapsed = collapse_line(&line);
        if collapsed.is_empty() {
            if !cur.is_empty() {
                paras.push(std::mem::take(&mut cur));
            }
        } else {
            if !cur.is_empty() {
                cur.push(('\n', Style::default()));
            }
            cur.extend(collapsed);
        }
    }
    if !cur.is_empty() {
        paras.push(cur);
    }
    paras
        .into_iter()
        .filter_map(|para| {
            let mut spans: Vec<Span> = Vec::new();
            for (c, s) in para {
                let txt = c.to_string();
                if let Some(last) = spans.last_mut() {
                    if style_of_span(last) == s {
                        last.text.push_str(&txt);
                        continue;
                    }
                }
                spans.push(Span {
                    text: txt,
                    bold: s.bold,
                    italic: s.italic,
                    strike: s.strike,
                    underline: s.underline,
                    mono: s.mono,
                });
            }
            // Merge single-char runs already handled; drop empties.
            spans.retain(|sp| !sp.text.is_empty());
            // Merge adjacent same-style runs (newline splits created many).
            let mut merged: Vec<Span> = Vec::new();
            for sp in spans {
                if let Some(last) = merged.last_mut() {
                    if style_of_span(last) == style_of_span(&sp) {
                        last.text.push_str(&sp.text);
                        continue;
                    }
                }
                merged.push(sp);
            }
            if merged.is_empty() {
                None
            } else {
                Some(merged)
            }
        })
        .collect()
}

/// Single-line normalisation for headings and list items: all runs of
/// whitespace (including `<br>`) collapse to one space.
fn single_line_spans(spans: Vec<Span>) -> Vec<Span> {
    let mut chars: Vec<(char, Style)> = Vec::new();
    for s in &spans {
        let st = style_of_span(s);
        for c in s.text.chars() {
            chars.push((if c == '\n' { ' ' } else { c }, st));
        }
    }
    let collapsed = collapse_line(&chars);
    let mut out: Vec<Span> = Vec::new();
    for (c, s) in collapsed {
        let txt = c.to_string();
        if let Some(last) = out.last_mut() {
            if style_of_span(last) == s {
                last.text.push_str(&txt);
                continue;
            }
        }
        out.push(Span {
            text: txt,
            bold: s.bold,
            italic: s.italic,
            strike: s.strike,
            underline: s.underline,
            mono: s.mono,
        });
    }
    out
}

/// List items preserve single `<br>` breaks but never split into paragraphs.
fn list_item_spans(spans: Vec<Span>) -> Vec<Span> {
    let mut chars: Vec<(char, Style)> = Vec::new();
    for s in &spans {
        let st = style_of_span(s);
        for c in s.text.chars() {
            chars.push((c, st));
        }
    }
    let mut lines: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    for (c, s) in chars {
        if c == '\n' {
            lines.push(Vec::new());
        } else {
            lines.last_mut().unwrap().push((c, s));
        }
    }
    let mut joined: Vec<(char, Style)> = Vec::new();
    for line in lines {
        let collapsed = collapse_line(&line);
        if collapsed.is_empty() {
            continue;
        }
        if !joined.is_empty() {
            joined.push(('\n', Style::default()));
        }
        joined.extend(collapsed);
    }
    let mut out: Vec<Span> = Vec::new();
    for (c, s) in joined {
        let txt = c.to_string();
        if let Some(last) = out.last_mut() {
            if style_of_span(last) == s {
                last.text.push_str(&txt);
                continue;
            }
        }
        out.push(Span {
            text: txt,
            bold: s.bold,
            italic: s.italic,
            strike: s.strike,
            underline: s.underline,
            mono: s.mono,
        });
    }
    out
}

/// Raw text for `<pre>`: significant whitespace kept, tags ignored.
fn pre_text(el: scraper::ElementRef) -> String {
    use scraper::node::Node;
    fn walk(el: scraper::ElementRef, out: &mut String) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => out.push_str(t),
                Node::Element(e) if e.name() == "br" => out.push('\n'),
                Node::Element(e)
                    if e.name() == "img" || e.name() == "image" || e.name() == "svg" => {}
                Node::Element(_) => {
                    if let Some(inner) = scraper::ElementRef::wrap(child) {
                        walk(inner, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut raw = String::new();
    walk(el, &mut raw);
    let decoded = html_escape::decode_html_entities(&raw).into_owned();
    decoded.replace('\r', "")
}

/// Flush accumulated inline spans into paragraph blocks. Blank lines (from
/// `<br><br>` or block boundaries) start new paragraphs; single newlines
/// (from a lone `<br>`) are preserved as line breaks inside the block.
fn push_text_blocks(pending: &mut Vec<Span>, out: &mut Vec<Block>) {
    if spans_plain(pending).trim().is_empty() {
        pending.clear();
        return;
    }
    let spans = std::mem::take(pending);
    for para in spans_to_paragraphs(spans) {
        out.push(Block::Paragraph(para));
    }
    pending.clear();
}

fn collect_blocks<F>(parent: scraper::ElementRef, out: &mut Vec<Block>, lookup: &mut F)
where
    F: FnMut(&str) -> Option<Vec<u8>>,
{
    use scraper::node::Node;
    let mut pending: Vec<Span> = Vec::new();
    for child in parent.children() {
        match child.value() {
            Node::Text(t) => {
                push_span(&mut pending, squash_newlines(t), Style::default());
            }
            Node::Element(e) if e.name() == "br" => {
                pending.push(Span {
                    text: "\n".to_string(),
                    ..Default::default()
                });
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
                    collect_element_spans(inner, Style::default(), &mut pending);
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
        let spans = collect_inline_spans(parent);
        if !spans_plain(&spans).trim().is_empty() {
            for para in spans_to_paragraphs(spans) {
                out.push(Block::Paragraph(para));
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
        "h1" => {
            let spans = single_line_spans(collect_inline_spans(el));
            if !spans_plain(&spans).trim().is_empty() {
                out.push(Block::Heading(1, spans));
            }
        }
        "h2" => {
            let spans = single_line_spans(collect_inline_spans(el));
            if !spans_plain(&spans).trim().is_empty() {
                out.push(Block::Heading(2, spans));
            }
        }
        "h3" => {
            let spans = single_line_spans(collect_inline_spans(el));
            if !spans_plain(&spans).trim().is_empty() {
                out.push(Block::Heading(3, spans));
            }
        }
        "h4" | "h5" | "h6" => {
            let spans = single_line_spans(collect_inline_spans(el));
            if !spans_plain(&spans).trim().is_empty() {
                out.push(Block::Heading(4, spans));
            }
        }
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
        "pre" => {
            let t = pre_text(el);
            if !t.trim().is_empty() {
                out.push(Block::Pre(t));
            }
        }
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
                let spans = list_item_spans(collect_inline_spans(li));
                if !spans_plain(&spans).trim().is_empty() {
                    out.push(Block::ListItem(spans));
                    any = true;
                }
            }
            if !any {
                // Malformed list without direct items: recurse instead of dropping.
                collect_blocks(el, out, lookup);
            }
        }
        "li" => {
            let spans = list_item_spans(collect_inline_spans(el));
            if !spans_plain(&spans).trim().is_empty() {
                out.push(Block::ListItem(spans));
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
                let spans = collect_inline_spans(el);
                let mut pending = spans;
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
pub(crate) fn resolve_resource(
    doc: &mut EpubDoc,
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
    let n = match b {
        Block::Heading(_, s) | Block::Paragraph(s) | Block::Quote(s) | Block::ListItem(s) => {
            spans_len(s)
        }
        Block::Pre(t) => t.chars().count(),
        Block::Image { .. } => 0,
    };
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

/// Split styled spans into chunks of at most `max_chars` plain characters,
/// preserving per-span flags. Cuts prefer whitespace; newline characters are
/// treated as whitespace boundaries and never start a chunk. Always makes
/// progress.
pub fn chunk_spans(spans: &[Span], max_chars: usize) -> Vec<Vec<Span>> {
    let max_chars = max_chars.max(1);
    // Flatten to chars with styles.
    let mut chars: Vec<(char, Style)> = Vec::new();
    for s in spans {
        let st = style_of_span(s);
        for c in s.text.chars() {
            chars.push((c, st));
        }
    }
    if chars.len() <= max_chars {
        return vec![spans.to_vec()];
    }
    let mut chunks: Vec<Vec<Span>> = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + max_chars).min(chars.len());
        if end < chars.len() {
            let mut cut = end;
            while cut > start && !chars[cut].0.is_whitespace() && end - cut < max_chars / 2 {
                cut -= 1;
            }
            if cut > start && chars[cut].0.is_whitespace() {
                end = cut;
            }
        }
        // Build spans for [start, end).
        let mut out: Vec<Span> = Vec::new();
        for (c, st) in &chars[start..end] {
            let txt = c.to_string();
            if let Some(last) = out.last_mut() {
                if style_of_span(last) == *st {
                    last.text.push_str(&txt);
                    continue;
                }
            }
            out.push(Span {
                text: txt,
                bold: st.bold,
                italic: st.italic,
                strike: st.strike,
                underline: st.underline,
                mono: st.mono,
            });
        }
        // Drop whitespace-only chunks to guarantee progress.
        if spans_plain(&out).trim().is_empty() {
            start = end.max(start + 1);
            continue;
        }
        chunks.push(out);
        start = end;
        while start < chars.len() && chars[start].0.is_whitespace() {
            start += 1;
        }
    }
    if chunks.is_empty() {
        // Fallback: trimmed single chunk preserving the first style.
        let trimmed = spans_plain(spans).trim().to_string();
        if !trimmed.is_empty() {
            chunks.push(plain_spans(&trimmed));
        }
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

    fn texts(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .map(|b| match b {
                Block::Heading(_, s)
                | Block::Paragraph(s)
                | Block::Quote(s)
                | Block::ListItem(s) => spans_plain(s),
                Block::Pre(t) => t.clone(),
                Block::Image { alt, .. } => alt.clone(),
            })
            .collect()
    }

    fn spans_of(block: &Block) -> &[Span] {
        match block {
            Block::Heading(_, s) | Block::Paragraph(s) | Block::Quote(s) | Block::ListItem(s) => s,
            Block::Pre(_) | Block::Image { .. } => &[],
        }
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
        assert_eq!(
            texts(&blocks),
            vec!["First.".to_string(), "Second.".to_string()]
        );
    }

    #[test]
    fn plain_paragraphs_stay_separate() {
        let blocks = html_to_blocks("<p>One.</p><p>Two.</p><p>Three.</p>", &mut |_: &str| None);
        assert_eq!(
            texts(&blocks),
            vec!["One.".to_string(), "Two.".to_string(), "Three.".to_string()]
        );
    }

    #[test]
    fn double_br_splits_paragraphs() {
        let blocks = html_to_blocks("<p>line one<br><br>line two</p>", &mut |_: &str| None);
        assert_eq!(
            texts(&blocks),
            vec!["line one".to_string(), "line two".to_string()]
        );
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
        assert_eq!(
            texts(&blocks),
            vec![
                "Intro".to_string(),
                "Body.".to_string(),
                "Tail.".to_string()
            ]
        );
    }

    #[test]
    fn inline_markup_stays_in_flow() {
        let blocks = html_to_blocks("<p>foo <b>bar</b> baz</p>", &mut |_: &str| None);
        assert_eq!(texts(&blocks), vec!["foo bar baz".to_string()]);
    }

    #[test]
    fn inline_formatting_flags_survive() {
        let blocks = html_to_blocks("<p>foo <b>bar</b> <i>baz</i></p>", &mut |_: &str| None);
        assert_eq!(blocks.len(), 1);
        let spans = spans_of(&blocks[0]);
        assert_eq!(spans_plain(spans), "foo bar baz");
        let bar = spans.iter().find(|s| s.text.contains("bar")).unwrap();
        assert!(bar.bold);
        assert!(!bar.italic);
        let baz = spans.iter().find(|s| s.text.contains("baz")).unwrap();
        assert!(baz.italic);
        assert!(!baz.bold);
    }

    #[test]
    fn underline_strike_mono_and_links() {
        let blocks = html_to_blocks(
            "<p><u>u</u> <s>s</s> <del>d</del> <code>c</code> <a href=\"x\">link</a></p>",
            &mut |_: &str| None,
        );
        assert_eq!(blocks.len(), 1);
        let spans = spans_of(&blocks[0]);
        let find = |t: &str| spans.iter().find(|s| s.text.contains(t)).unwrap().clone();
        assert!(find("u").underline);
        assert!(find("s").strike);
        assert!(find("d").strike);
        assert!(find("c").mono);
        assert!(find("link").underline);
    }

    #[test]
    fn nested_formatting_combines() {
        let blocks = html_to_blocks("<p><b><i>both</i></b></p>", &mut |_: &str| None);
        let spans = spans_of(&blocks[0]);
        assert_eq!(spans.len(), 1);
        assert!(spans[0].bold && spans[0].italic);
    }

    #[test]
    fn chunk_spans_preserves_flags_and_text() {
        let spans = vec![
            Span {
                text: "aaa bbb ccc".to_string(),
                bold: true,
                ..Default::default()
            },
            Span {
                text: " ddd".to_string(),
                italic: true,
                ..Default::default()
            },
        ];
        let chunks = chunk_spans(&spans, 7);
        assert!(chunks.iter().all(|c| spans_len(c) <= 7));
        let joined: String = chunks.iter().map(|c| spans_plain(c)).collect();
        assert_eq!(joined.replace(' ', ""), "aaabbbcccddd");
        // Bold survives on the first chunk, italic on the last.
        assert!(chunks.first().unwrap().iter().any(|s| s.bold));
        assert!(chunks.last().unwrap().iter().any(|s| s.italic));
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
        assert_eq!(texts(&blocks), vec!["Before. After.".to_string()]);
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

    fn anchor_texts(blocks: &[Block]) -> Vec<String> {
        blocks.iter().map(anchor_text).collect()
    }

    #[test]
    fn anchor_exact_and_fuzzy() {
        let blocks = vec![
            Block::Paragraph(plain_spans("The quick brown fox jumps.")),
            Block::Paragraph(plain_spans("Lazy dogs sleep all day.")),
        ];
        let texts = anchor_texts(&blocks);
        // Exact offsets reproduce the quote.
        let quote = anchor_range_text(&texts, 0, 4, 0, 9);
        assert_eq!(quote, "quick");
        let found = find_anchor(&texts, 0, 4, 0, 9, &quote, "The ", " brown").unwrap();
        assert_eq!(found, (0, 4, 0, 9));
        // Edited text: exact fails, fuzzy finds the surviving word.
        let edited = vec![
            "The quick red fox jumps.".to_string(),
            "Lazy dogs sleep all day.".to_string(),
        ];
        let found = find_anchor(&edited, 0, 4, 0, 9, &quote, "The ", " brown").unwrap();
        assert_eq!((found.0, found.2), (0, 0));
        assert_eq!(slice_chars(&edited[0], found.1, found.3), "quick");
        // Empty quote never resolves.
        assert!(find_anchor(&texts, 0, 0, 0, 3, "", "", "").is_none());
    }

    #[test]
    fn anchor_multi_block_and_orphan() {
        let blocks = vec![
            Block::Paragraph(plain_spans("Alpha beta gamma.")),
            Block::Paragraph(plain_spans("Delta epsilon zeta.")),
            Block::Paragraph(plain_spans("Eta theta iota.")),
        ];
        let texts = anchor_texts(&blocks);
        let quote = anchor_range_text(&texts, 0, 6, 2, 9);
        assert!(quote.contains("beta") && quote.contains("theta"));
        let found = find_anchor(&texts, 0, 6, 2, 9, &quote, "Alpha ", " iota.").unwrap();
        assert_eq!((found.0, found.2), (0, 2));
        // Fully rewritten chapter orphans the quote.
        let gone = vec!["Nothing alike here.".to_string()];
        assert!(find_anchor(&gone, 0, 6, 2, 9, &quote, "Alpha ", " iota.").is_none());
    }

    /// Minimal 3-chapter EPUB on disk (no NCX, so no nav TOC).
    fn fixture_epub(path: &std::path::Path) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file(
            "mimetype",
            opts.compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"application/epub+zip").unwrap();
        zip.start_file("META-INF/container.xml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();
        zip.start_file("OEBPS/content.opf", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><package version="3.0" xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>T</dc:title><dc:creator>A</dc:creator></metadata><manifest><item id="c0" href="c0.xhtml" media-type="application/xhtml+xml"/><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/><item id="c2" href="c2.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c0"/><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#).unwrap();
        for (name, body) in [
            (
                "OEBPS/c0.xhtml",
                "<h1>Title One</h1><p>First chapter text here.</p>",
            ),
            ("OEBPS/c1.xhtml", "<p>Second chapter, no heading.</p>"),
            (
                "OEBPS/c2.xhtml",
                "<h2>Third Part</h2><p>Third chapter text here.</p>",
            ),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(
                format!(
                    r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body>{body}</body></html>"#
                )
                .as_bytes(),
            )
            .unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn lazy_single_chapter_matches_full_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.epub");
        fixture_epub(&path);
        let (toc_full, chapters_full) = load_book(&path).unwrap();
        assert_eq!(chapters_full.len(), 3);
        let (toc_skel, hrefs) = book_skeleton(&path).unwrap();
        assert_eq!(hrefs.len(), chapters_full.len());
        // Skeleton is stable without chapter bodies: same hrefs. It carries
        // only the nav TOC (empty here); the heading fallback is built by the
        // reader once all chapters are in.
        for (i, ch) in chapters_full.iter().enumerate() {
            assert_eq!(hrefs[i], ch.href);
        }
        assert!(toc_skel.is_empty());
        assert_eq!(toc_full.len(), 2);
        // Each lazy chapter matches its full-parse counterpart exactly
        // (labels incl. heading adoption, hrefs, block text).
        for (i, full) in chapters_full.iter().enumerate() {
            let ch = load_chapter(&path, i, &toc_skel).unwrap();
            assert_eq!(ch.index, i);
            assert_eq!(ch.label, full.label);
            assert_eq!(ch.href, full.href);
            assert_eq!(anchor_texts(&ch.blocks), anchor_texts(&full.blocks));
        }
        // Heading-led chapter adopts its title; heading-less keeps Chapter N.
        assert_eq!(chapters_full[0].label, "Title One");
        assert_eq!(chapters_full[2].label, "Third Part");
    }
}
