//! Minimal MIT-licensed EPUB reader replacing the GPL `epub` crate.
//!
//! Implements only the subset this project uses: spine order, chapter HTML,
//! NCX + EPUB3 nav TOC, Dublin Core / calibre metadata, cover lookup, and
//! manifest resource reads. Built on `zip` (already a dependency) plus
//! `roxmltree` (MIT/Apache) for OPF/container/NCX XML.
//!
//! Intentionally file-backed (`EpubDoc::new(path)`) mirroring the old API so
//! call sites stay close to their previous shape.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// TOC entry: label plus archive path of the target document.
#[derive(Clone, Debug)]
pub struct NavPoint {
    pub label: String,
    pub content: PathBuf,
    pub children: Vec<NavPoint>,
    pub play_order: Option<usize>,
}

/// Dublin Core / OPF metadata item (top-level only; refinements ignored —
/// no caller reads them, matching the old crate's observable behavior).
#[derive(Clone, Debug)]
pub struct MetadataItem {
    pub property: String,
    pub value: String,
    pub id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ResourceItem {
    pub path: PathBuf,
    pub mime: String,
    pub properties: Option<String>,
}

#[derive(Clone, Debug)]
struct SpineItem {
    idref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EpubVersion {
    V2,
    V3,
    Unknown,
}

/// File-backed EPUB document. Each resource read re-opens the zip; books are
/// small and opens are cheap relative to HTML parsing.
#[derive(Clone, Debug)]
pub struct EpubDoc {
    archive_path: PathBuf,
    version: EpubVersion,
    spine: Vec<SpineItem>,
    pub resources: HashMap<String, ResourceItem>,
    pub toc: Vec<NavPoint>,
    pub metadata: Vec<MetadataItem>,
    current: usize,
}

impl EpubDoc {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let container = read_zip_entry(&path, "META-INF/container.xml")
            .ok_or_else(|| "missing META-INF/container.xml".to_string())?;
        let container = String::from_utf8_lossy(&container).into_owned();
        let root_file = root_file_from_container(&container)?;
        let root_base = root_file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();

        let opf_raw = read_zip_entry(&path, &fwd(&root_file))
            .ok_or_else(|| format!("missing OPF {}", fwd(&root_file)))?;
        let opf = String::from_utf8_lossy(&opf_raw).into_owned();
        let parsed = parse_opf(&opf, &root_base)?;

        let mut doc = Self {
            archive_path: path,
            version: parsed.version,
            spine: parsed.spine,
            resources: parsed.resources,
            toc: Vec::new(),
            metadata: parsed.metadata,
            current: 0,
        };
        doc.toc = parsed
            .toc_ncx_id
            .and_then(|id| doc.parse_ncx(&id))
            .unwrap_or_default();
        if doc.toc.is_empty() {
            // EPUB3 nav-only books have no NCX; the old crate left `toc`
            // empty here and callers fell back to headings. Prefer the nav
            // document when present (strictly more TOC, same shape).
            if let Some(nav_toc) = doc.parse_nav() {
                doc.toc = nav_toc;
            }
        }
        doc.toc.sort_by_key(|a| a.play_order);
        Ok(doc)
    }

    pub fn get_num_chapters(&self) -> usize {
        self.spine.len()
    }

    pub fn set_current_chapter(&mut self, n: usize) -> bool {
        if n >= self.spine.len() {
            false
        } else {
            self.current = n;
            true
        }
    }

    fn current_idref(&self) -> Option<String> {
        self.spine.get(self.current).map(|s| s.idref.clone())
    }

    pub fn get_current_path(&self) -> Option<PathBuf> {
        let id = self.current_idref()?;
        self.resources.get(&id).map(|r| r.path.clone())
    }

    pub fn get_current_str(&mut self) -> Option<(String, String)> {
        let id = self.current_idref()?;
        let (bytes, mime) = self.get_resource(&id)?;
        Some((String::from_utf8_lossy(&bytes).into_owned(), mime))
    }

    pub fn get_title(&self) -> Option<String> {
        self.metadata
            .iter()
            .find(|m| m.property == "title")
            .map(|m| m.value.clone())
    }

    pub fn get_cover(&mut self) -> Option<(Vec<u8>, String)> {
        // Mirrors the old crate: EPUB3 uses `cover-image`, EPUB2 uses
        // `<meta name="cover" content="id"/>`; each falls back to the other
        // so mixed-version books still resolve.
        let cover_image = self.resources.iter().find_map(|(id, r)| {
            r.properties.as_ref().and_then(|p| {
                p.split_ascii_whitespace()
                    .any(|t| t == "cover-image")
                    .then(|| id.clone())
            })
        });
        let meta_cover = self
            .metadata
            .iter()
            .find(|m| m.property == "cover")
            .map(|m| m.value.clone());
        let (first, second) = if self.version == EpubVersion::V3 {
            (cover_image, meta_cover)
        } else {
            (meta_cover, cover_image)
        };
        for id in [first, second].into_iter().flatten() {
            if let Some(hit) = self.get_resource(&id) {
                return Some(hit);
            }
        }
        None
    }

    fn get_resource(&mut self, id: &str) -> Option<(Vec<u8>, String)> {
        let (path, mime) = self
            .resources
            .get(id)
            .map(|r| (r.path.clone(), r.mime.clone()))?;
        let bytes = self.get_resource_by_path(&path)?;
        Some((bytes, mime))
    }

    pub fn get_resource_by_path<P: AsRef<Path>>(&mut self, path: P) -> Option<Vec<u8>> {
        read_zip_entry(&self.archive_path, &fwd(path.as_ref()))
    }

    /// Map a TOC `content` path to a spine index. Fragments/queries are
    /// stripped so `ch1.xhtml#sec` still resolves (the old crate required an
    /// exact match and returned `None` there).
    pub fn resource_uri_to_chapter(&self, uri: &Path) -> Option<usize> {
        let want = strip_fragment(&fwd(uri));
        if want.is_empty() {
            return None;
        }
        // Exact archive-path match against spine order.
        for (i, item) in self.spine.iter().enumerate() {
            if let Some(r) = self.resources.get(&item.idref) {
                if strip_fragment(&fwd(&r.path)) == want {
                    return Some(i);
                }
            }
        }
        None
    }

    fn parse_ncx(&mut self, ncx_id: &str) -> Option<Vec<NavPoint>> {
        let ncx_path = self.resources.get(ncx_id)?.path.clone();
        let base = ncx_path.parent().map(Path::to_path_buf).unwrap_or_default();
        let raw = self.get_resource_by_path(&ncx_path)?;
        let text = String::from_utf8_lossy(&raw).into_owned();
        let doc = roxmltree::Document::parse(&text).ok()?;
        let nav_map = find_descendant(doc.root(), "navMap")?;
        let mut points = navpoints_from_parent(nav_map, &base);
        sort_navpoints(&mut points);
        Some(points)
    }

    fn parse_nav(&mut self) -> Option<Vec<NavPoint>> {
        let (nav_path, nav_id) = self.resources.iter().find_map(|(id, r)| {
            r.properties.as_ref().and_then(|p| {
                p.split_ascii_whitespace()
                    .any(|t| t == "nav")
                    .then(|| (r.path.clone(), id.clone()))
            })
        })?;
        let _ = nav_id;
        let base = nav_path.parent().map(Path::to_path_buf).unwrap_or_default();
        let raw = self.get_resource_by_path(&nav_path)?;
        let html = String::from_utf8_lossy(&raw).into_owned();
        navpoints_from_html(&html, &base)
    }
}

// ---------------------------------------------------------------------------
// zip + path helpers
// ---------------------------------------------------------------------------

fn read_zip_entry(archive: &Path, name: &str) -> Option<Vec<u8>> {
    let file = File::open(archive).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    // Names in zips are forward-slash separated; also try a `./`-stripped form.
    let clean = name.trim_start_matches("./");
    for candidate in [name, clean] {
        if let Ok(mut entry) = zip.by_name(candidate) {
            let mut out = Vec::new();
            if entry.read_to_end(&mut out).is_ok() {
                return Some(out);
            }
            return None;
        }
    }
    None
}

/// Forward-slash form for zip lookups and cross-platform comparisons.
fn fwd(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn strip_fragment(s: &str) -> String {
    s.split(['#', '?']).next().unwrap_or("").to_string()
}

/// Lexically join `href` onto `base` (archive-rooted when `href` starts with
/// `/`), resolving `.`/`..` and dropping query/fragment. Returns a
/// forward-slash archive path.
fn join_href(base: &Path, href: &str) -> String {
    let href = strip_fragment(href.trim());
    if href.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = if href.starts_with('/') {
        Vec::new()
    } else {
        fwd(base)
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    let rel = href.strip_prefix('/').unwrap_or(&href);
    for comp in rel.split('/') {
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

// ---------------------------------------------------------------------------
// container / OPF
// ---------------------------------------------------------------------------

fn root_file_from_container(xml: &str) -> Result<PathBuf, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("bad container.xml: {e}"))?;
    doc.root()
        .descendants()
        .filter(|n| n.is_element())
        .find(|n| n.tag_name().name() == "rootfile")
        .and_then(|n| n.attribute("full-path"))
        .map(PathBuf::from)
        .ok_or_else(|| "container.xml has no rootfile".to_string())
}

struct ParsedOpf {
    version: EpubVersion,
    spine: Vec<SpineItem>,
    resources: HashMap<String, ResourceItem>,
    metadata: Vec<MetadataItem>,
    toc_ncx_id: Option<String>,
}

fn parse_opf(xml: &str, root_base: &Path) -> Result<ParsedOpf, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("bad OPF: {e}"))?;
    let package = doc.root_element();
    let version = match package.attribute("version") {
        Some("2.0") => EpubVersion::V2,
        Some("3.0") => EpubVersion::V3,
        _ => EpubVersion::Unknown,
    };

    let mut resources = HashMap::new();
    if let Some(manifest) = find_child(package, "manifest") {
        for item in manifest.children().filter(|n| n.is_element()) {
            if item.tag_name().name() != "item" {
                continue;
            }
            let (Some(id), Some(href), Some(mime)) = (
                item.attribute("id"),
                item.attribute("href"),
                item.attribute("media-type"),
            ) else {
                continue;
            };
            let joined = join_href(root_base, href);
            if joined.is_empty() {
                continue;
            }
            resources.insert(
                id.to_string(),
                ResourceItem {
                    path: PathBuf::from(joined),
                    mime: mime.to_string(),
                    properties: item.attribute("properties").map(str::to_string),
                },
            );
        }
    }

    let mut spine = Vec::new();
    let mut toc_ncx_id = None;
    if let Some(spine_el) = find_child(package, "spine") {
        toc_ncx_id = spine_el.attribute("toc").map(str::to_string);
        for item in spine_el.children().filter(|n| n.is_element()) {
            if item.tag_name().name() != "itemref" {
                continue;
            }
            if let Some(idref) = item.attribute("idref") {
                spine.push(SpineItem {
                    idref: idref.to_string(),
                });
            }
        }
    }
    // Fallback when `spine toc=` is absent: the NCX manifest item itself.
    if toc_ncx_id.is_none() {
        toc_ncx_id = resources
            .iter()
            .find(|(_, r)| r.mime == "application/x-dtbncx+xml")
            .map(|(id, _)| id.clone());
    }

    let mut metadata = Vec::new();
    if let Some(meta_el) = find_child(package, "metadata") {
        for node in meta_el.children().filter(|n| n.is_element()) {
            let name = node.tag_name().name().to_string();
            let ns = node.tag_name().namespace().unwrap_or("");
            if ns == "http://purl.org/dc/elements/1.1/" {
                metadata.push(MetadataItem {
                    property: name,
                    value: node.text().unwrap_or_default().to_string(),
                    id: node.attribute("id").map(str::to_string),
                });
            } else if name == "meta" {
                if let Some(property) = node.attribute("property") {
                    // EPUB3 `<meta property=...>`; refinements (`refines=#..`)
                    // are sub-expressions, not top-level metadata.
                    if node.attribute("refines").is_none() {
                        metadata.push(MetadataItem {
                            property: property.to_string(),
                            value: node.text().unwrap_or_default().to_string(),
                            id: node.attribute("id").map(str::to_string),
                        });
                    }
                } else if let (Some(nm), Some(content)) =
                    (node.attribute("name"), node.attribute("content"))
                {
                    // EPUB2 / calibre `<meta name= content=>`.
                    metadata.push(MetadataItem {
                        property: nm.to_string(),
                        value: content.to_string(),
                        id: None,
                    });
                } else if ns.is_empty() {
                    // Unprefixed heuristic for OPFs missing namespace decls.
                    if let Some(property) = node.attribute("property") {
                        if node.attribute("refines").is_none() {
                            metadata.push(MetadataItem {
                                property: property.to_string(),
                                value: node.text().unwrap_or_default().to_string(),
                                id: node.attribute("id").map(str::to_string),
                            });
                        }
                    }
                }
            } else if ns.is_empty() {
                // Tolerate OPFs without `xmlns:dc`: known Dublin Core names.
                const DC: &[&str] = &[
                    "title",
                    "creator",
                    "contributor",
                    "publisher",
                    "subject",
                    "description",
                    "identifier",
                    "source",
                    "language",
                    "date",
                    "collection",
                ];
                if DC.contains(&name.as_str()) {
                    metadata.push(MetadataItem {
                        property: name,
                        value: node.text().unwrap_or_default().to_string(),
                        id: node.attribute("id").map(str::to_string),
                    });
                }
            }
        }
    }

    Ok(ParsedOpf {
        version,
        spine,
        resources,
        metadata,
        toc_ncx_id,
    })
}

fn find_child<'a>(parent: roxmltree::Node<'a, 'a>, local: &str) -> Option<roxmltree::Node<'a, 'a>> {
    parent
        .children()
        .filter(|n| n.is_element())
        .find(|n| n.tag_name().name() == local)
}

fn find_descendant<'a>(
    root: roxmltree::Node<'a, 'a>,
    local: &str,
) -> Option<roxmltree::Node<'a, 'a>> {
    root.descendants()
        .filter(|n| n.is_element())
        .find(|n| n.tag_name().name() == local)
}

// ---------------------------------------------------------------------------
// NCX
// ---------------------------------------------------------------------------

fn navpoints_from_parent(parent: roxmltree::Node, base: &Path) -> Vec<NavPoint> {
    let mut out = Vec::new();
    for nav in parent.children().filter(|n| n.is_element()) {
        if nav.tag_name().name() != "navPoint" {
            continue;
        }
        let play_order = nav
            .attribute("playOrder")
            .and_then(|s| s.parse::<usize>().ok());
        let content = nav
            .children()
            .filter(|n| n.is_element())
            .find(|n| n.tag_name().name() == "content")
            .and_then(|c| c.attribute("src"))
            .map(|src| PathBuf::from(join_href(base, src)));
        let label = nav
            .children()
            .filter(|n| n.is_element())
            .find(|n| n.tag_name().name() == "navLabel")
            .and_then(|l| {
                l.children()
                    .filter(|n| n.is_element())
                    .find(|n| n.tag_name().name() == "text")
                    .and_then(|t| t.text())
                    .or_else(|| l.text())
            })
            .unwrap_or_default()
            .trim()
            .to_string();
        let (Some(content),) = (content,) else {
            continue;
        };
        if join_href(Path::new(""), &content.to_string_lossy()).is_empty()
            && fwd(&content).is_empty()
        {
            continue;
        }
        out.push(NavPoint {
            label,
            content,
            children: navpoints_from_parent(nav, base),
            play_order,
        });
    }
    out
}

fn sort_navpoints(points: &mut [NavPoint]) {
    points.sort_by_key(|a| a.play_order);
    for p in points.iter_mut() {
        sort_navpoints(&mut p.children);
    }
}

// ---------------------------------------------------------------------------
// EPUB3 nav document (XHTML) via `scraper`
// ---------------------------------------------------------------------------

fn navpoints_from_html(html: &str, base: &Path) -> Option<Vec<NavPoint>> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let nav_sel = Selector::parse("nav").ok()?;
    let navs: Vec<_> = doc.select(&nav_sel).collect();
    if navs.is_empty() {
        return None;
    }
    // Prefer the TOC nav; otherwise the first nav containing links.
    let picked = navs
        .iter()
        .find(|n| {
            ["epub:type", "type"]
                .iter()
                .filter_map(|a| n.value().attr(a))
                .any(|v| v.split_whitespace().any(|t| t.eq_ignore_ascii_case("toc")))
        })
        .or_else(|| navs.first())
        .cloned()?;
    let ol_sel = Selector::parse("ol").ok()?;
    let ol = picked.select(&ol_sel).next()?;
    let points = navpoints_from_ol(ol, base);
    if points.is_empty() {
        None
    } else {
        Some(points)
    }
}

fn navpoints_from_ol(ol: scraper::ElementRef, base: &Path) -> Vec<NavPoint> {
    use scraper::ElementRef;
    let mut out = Vec::new();
    for li in ol.children().filter_map(ElementRef::wrap) {
        if li.value().name() != "li" {
            continue;
        }
        // First anchor in document order within this `li` (before nested `ol`).
        let mut href: Option<String> = None;
        let mut label = String::new();
        for node in li.descendants() {
            let Some(el) = ElementRef::wrap(node) else {
                continue;
            };
            if el.value().name() == "ol" {
                break; // nested list belongs to children
            }
            if el.value().name() == "a" && href.is_none() {
                href = el.value().attr("href").map(str::to_string);
                label = el.text().collect::<Vec<_>>().join(" ");
                break;
            }
        }
        let children: Vec<NavPoint> = li
            .children()
            .filter_map(ElementRef::wrap)
            .filter(|e| e.value().name() == "ol")
            .flat_map(|nested| navpoints_from_ol(nested, base))
            .collect();
        if let Some(href) = href {
            let joined = join_href(base, &href);
            if joined.is_empty() {
                if children.is_empty() {
                    continue;
                }
                // Container entry without href: splice children up.
                out.extend(children);
                continue;
            }
            let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push(NavPoint {
                label,
                content: PathBuf::from(joined),
                children,
                play_order: None,
            });
        } else if !children.is_empty() {
            out.extend(children);
        }
    }
    out
}
