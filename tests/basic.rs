use marginalia::epub_utils;
use marginalia::library::{Library, SortKey};
use std::io::Write;
use std::path::Path;

fn make_epub(path: &Path, title: &str, author: &str) {
    make_epub_full(
        path,
        title,
        author,
        1,
        Some("http://example.com/fic/123"),
        "",
    );
}

fn make_epub_n(path: &Path, title: &str, author: &str, paras: usize) {
    make_epub_full(
        path,
        title,
        author,
        paras,
        Some("http://example.com/fic/123"),
        "",
    );
}

/// Direct-AO3-style download: no URL identifier, work link in the preface.
fn make_epub_ao3(path: &Path, title: &str, author: &str, work_id: u32) {
    make_epub_full(
        path,
        title,
        author,
        1,
        None,
        &format!("Posted originally on the Archive of Our Own at http://archiveofourown.org/works/{work_id}."),
    );
}

fn make_epub_full(
    path: &Path,
    title: &str,
    author: &str,
    paras: usize,
    identifier: Option<&str>,
    preface: &str,
) {
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
    zip.write_all(
        br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    )
    .unwrap();
    let ident = identifier
        .map(|id| format!("<dc:identifier>{id}</dc:identifier>"))
        .unwrap_or_default();
    let opf = format!(
        r#"<?xml version="1.0"?><package version="3.0" xmlns="http://www.idpf.org/2007/opf" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title><dc:creator>{author}</dc:creator><dc:language>en</dc:language>{ident}</metadata><manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="ncx"><itemref idref="ch1"/></spine></package>"#
    );
    zip.start_file("OEBPS/content.opf", opts).unwrap();
    zip.write_all(opf.as_bytes()).unwrap();
    zip.start_file("OEBPS/ch1.xhtml", opts).unwrap();
    let mut body = format!("<h1>{title}</h1><p>Hello from {author}.</p>");
    if !preface.is_empty() {
        body = format!("<p>{preface}</p>{body}");
    }
    for i in 0..paras {
        body.push_str(&format!(
            "<p>Filler paragraph {i} with some words to grow the file.</p>"
        ));
    }
    zip.write_all(
        format!(
            r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body>{body}</body></html>"#
        )
        .as_bytes(),
    )
    .unwrap();
    zip.start_file("OEBPS/toc.ncx", opts).unwrap();
    zip.write_all(
        br#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><docTitle><text>T</text></docTitle><navMap><navPoint id="n1" playOrder="1"><navLabel><text>Ch 1</text></navLabel><content src="ch1.xhtml"/></navPoint></navMap></ncx>"#,
    )
    .unwrap();
    zip.finish().unwrap();
}

#[test]
fn add_list_read_progress() {
    let libdir = tempfile::tempdir().unwrap();
    let lib = Library::open(libdir.path().join("lib")).expect("open lib");

    let srcdir = tempfile::tempdir().unwrap();
    let epub1 = srcdir.path().join("a.epub");
    make_epub(&epub1, "Marginalia Test", "Jane Author");

    let outcome = lib.add_file(&epub1).expect("add");
    let book = match outcome {
        marginalia::library::AddOutcome::Added(b) => b,
        marginalia::library::AddOutcome::Duplicate(b) => b,
    };
    assert_eq!(book.title, "Marginalia Test");
    assert!(book.authors.contains("Jane"));
    assert_eq!(
        book.fanfic_url.as_deref(),
        Some("http://example.com/fic/123")
    );

    // Duplicate is detected.
    let dup = lib.add_file(&epub1).expect("add again");
    assert!(matches!(dup, marginalia::library::AddOutcome::Duplicate(_)));

    // List + search + sort.
    let all = lib.list("", SortKey::Title).expect("list");
    assert_eq!(all.len(), 1);
    assert_eq!(lib.list("marginalia", SortKey::Title).unwrap().len(), 1);
    assert_eq!(lib.list("nope", SortKey::Title).unwrap().len(), 0);

    // Reader parsing.
    let path = lib.abs_path(&book.file_path);
    let (toc, chapters) = epub_utils::load_book(&path).expect("load book");
    assert_eq!(chapters.len(), 1);
    assert!(!chapters[0].blocks.is_empty());
    assert!(!toc.is_empty());

    // Progress + reopen.
    lib.save_progress(book.id, 0, 0).expect("progress");
    let got = lib.get_by_id(book.id).unwrap().unwrap();
    assert_eq!(got.progress_chapter, 0);
    assert_eq!(got.progress_page, 0);
    lib.touch_opened(book.id).expect("touch");

    // Tracked (fanfic url present).
    assert_eq!(lib.tracked().unwrap().len(), 1);
}

#[test]
fn display_settings_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let lib = marginalia::library::Library::open(dir.path().join("lib")).unwrap();
    // Fresh library reports the new defaults.
    let d = lib.get_display_settings();
    assert_eq!(d.font_family, "Bitter");
    assert_eq!(
        (d.font_size, d.font_weight, d.max_width),
        (18.0, 450.0, 900.0)
    );
    assert_eq!(d.column_mode, "Auto");
    assert_eq!(d.theme, "default-dark");
    // Writes survive reopening.
    let mut custom = d.clone();
    custom.font_family = "Lora".into();
    custom.font_size = 20.0;
    custom.font_weight = 600.0;
    custom.max_width = 1000.0;
    custom.column_mode = "One".into();
    custom.theme = "gruvbox-dark".into();
    lib.save_display_settings(&custom).unwrap();
    let lib2 = marginalia::library::Library::open(dir.path().join("lib")).unwrap();
    assert_eq!(lib2.get_display_settings(), custom);
}

#[test]
fn legacy_dark_theme_flag_migrates() {
    // Libraries from before the theme picker stored a ui.dark_theme bool.
    let dir = tempfile::tempdir().unwrap();
    let lib = marginalia::library::Library::open(dir.path().join("lib")).unwrap();
    lib.set_setting("ui.dark_theme", "false").unwrap();
    assert_eq!(lib.get_display_settings().theme, "default-light");
    lib.set_setting("ui.dark_theme", "true").unwrap();
    assert_eq!(lib.get_display_settings().theme, "default-dark");
    // An explicit ui.theme row wins over the legacy flag.
    lib.set_setting("ui.theme", "dracula").unwrap();
    assert_eq!(lib.get_display_settings().theme, "dracula");
}

#[test]
fn size_and_progress_sorts() {
    use marginalia::library::AddOutcome;

    let libdir = tempfile::tempdir().unwrap();
    let lib = Library::open(libdir.path().join("lib")).expect("open lib");
    let srcdir = tempfile::tempdir().unwrap();

    let small = srcdir.path().join("small.epub");
    make_epub_n(&small, "Small Book", "Ann Author", 1);
    let big = srcdir.path().join("big.epub");
    make_epub_n(&big, "Big Book", "Zed Author", 60);

    let id_of = |o: AddOutcome| match o {
        AddOutcome::Added(b) => b.id,
        AddOutcome::Duplicate(b) => b.id,
    };
    let small_id = id_of(lib.add_file(&small).expect("add small"));
    let big_id = id_of(lib.add_file(&big).expect("add big"));

    // Size sorts biggest-first by default.
    let titles: Vec<String> = lib
        .list("", SortKey::Size)
        .expect("list")
        .into_iter()
        .map(|b| b.title)
        .collect();
    assert_eq!(
        titles,
        vec!["Big Book".to_string(), "Small Book".to_string()]
    );

    // Progress sorts most-read first; untouched books sink.
    lib.save_progress(big_id, 2, 0).expect("progress");
    lib.save_progress(small_id, 0, 0).expect("progress");
    let titles: Vec<String> = lib
        .list("", SortKey::Progress)
        .expect("list")
        .into_iter()
        .map(|b| b.title)
        .collect();
    assert_eq!(
        titles,
        vec!["Big Book".to_string(), "Small Book".to_string()]
    );
}

/// Multi-chapter fixture: `files` is (spine file, body html); `nav` is
/// (nav label, spine file) with one navPoint each.
fn make_epub_nav(path: &Path, files: &[(&str, &str)], nav: &[(&str, &str)]) {
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
    zip.write_all(
        br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    )
    .unwrap();
    let mut manifest = String::new();
    let mut spine = String::new();
    for (i, (name, _)) in files.iter().enumerate() {
        manifest.push_str(&format!(
            r#"<item id="f{i}" href="{name}" media-type="application/xhtml+xml"/>"#
        ));
        spine.push_str(&format!(r#"<itemref idref="f{i}"/>"#));
    }
    manifest.push_str(r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#);
    let opf = format!(
        r#"<?xml version="1.0"?><package version="3.0" xmlns="http://www.idpf.org/2007/opf" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>T</dc:title><dc:creator>A</dc:creator></metadata><manifest>{manifest}</manifest><spine toc="ncx">{spine}</spine></package>"#
    );
    zip.start_file("OEBPS/content.opf", opts).unwrap();
    zip.write_all(opf.as_bytes()).unwrap();
    for (name, body) in files {
        zip.start_file(format!("OEBPS/{name}"), opts).unwrap();
        zip.write_all(
            format!(
                r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body>{body}</body></html>"#
            )
            .as_bytes(),
        )
        .unwrap();
    }
    let mut navmap = String::new();
    for (i, (label, file)) in nav.iter().enumerate() {
        navmap.push_str(&format!(
            r#"<navPoint id="n{i}" playOrder="{i}"><navLabel><text>{label}</text></navLabel><content src="{file}"/></navPoint>"#
        ));
    }
    zip.start_file("OEBPS/toc.ncx", opts).unwrap();
    zip.write_all(
        format!(
            r#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><docTitle><text>T</text></docTitle><navMap>{navmap}</navMap></ncx>"#
        )
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap();
}

#[test]
fn unmapped_chapters_adopt_in_body_titles() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.epub");
    // Title page has no navPoint; ch2 has no heading at all.
    make_epub_nav(
        &p,
        &[
            ("title.xhtml", "<h1>My Great Fic</h1><p>By Me.</p>"),
            ("ch1.xhtml", "<h2>Real Start</h2><p>Body.</p>"),
            ("ch2.xhtml", "<p>No heading here.</p>"),
        ],
        &[("Real Start", "ch1.xhtml")],
    );
    let (toc, chapters) = epub_utils::load_book(&p).expect("load");
    assert_eq!(toc.len(), 1);
    assert_eq!(chapters.len(), 3);
    // Title page adopts its own h1 instead of a bogus positional label...
    assert_eq!(chapters[0].label, "My Great Fic");
    // ...and the adopted heading is removed so it won't render twice.
    assert!(matches!(
        chapters[0].blocks.first(),
        Some(marginalia::epub_utils::Block::Paragraph(_))
    ));
    // Mapped chapter keeps its TOC label.
    assert_eq!(chapters[1].label, "Real Start");
    // Heading-less chapter without a TOC entry keeps its positional label.
    assert_eq!(chapters[2].label, "Chapter 3");
}

#[test]
fn blank_toc_labels_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.epub");
    make_epub_nav(
        &p,
        &[("ch1.xhtml", "<h1>Actual Title</h1><p>Body.</p>")],
        // Whitespace-only nav label must not blank the chapter title.
        &[("   ", "ch1.xhtml")],
    );
    let (_, chapters) = epub_utils::load_book(&p).expect("load");
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].label, "Actual Title");
}

/// Fixture epub with a real 300x100 PNG illustration referenced via a
/// subdirectory path (`Text/ch1.xhtml` -> `../Images/pic.png`), which
/// exercises relative-path resolution through the manifest.
fn make_epub_with_image(path: &Path, png: &[u8]) {
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
    zip.write_all(
        br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    )
    .unwrap();
    let opf = r#"<?xml version="1.0"?><package version="3.0" xmlns="http://www.idpf.org/2007/opf" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Pic</dc:title><dc:creator>A</dc:creator></metadata><manifest><item id="ch" href="Text/ch1.xhtml" media-type="application/xhtml+xml"/><item id="img" href="Images/pic.png" media-type="image/png"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="ncx"><itemref idref="ch"/></spine></package>"#;
    zip.start_file("OEBPS/content.opf", opts).unwrap();
    zip.write_all(opf.as_bytes()).unwrap();
    zip.start_file("OEBPS/Text/ch1.xhtml", opts).unwrap();
    zip.write_all(
        br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><p>Top.</p><p><img src="../Images/pic.png" alt="A pic"/></p><p><img src="missing.png"/></p><p><img src="https://example.com/x.png"/></p><p><img src="pic.png" alt="Fallback"/></p><p>Bottom.</p></body></html>"#,
    )
    .unwrap();
    zip.start_file("OEBPS/Images/pic.png", opts).unwrap();
    zip.write_all(png).unwrap();
    // Present in the zip but NOT in the manifest: must be ignored.
    zip.start_file("OEBPS/Images/unlisted.png", opts).unwrap();
    zip.write_all(png).unwrap();
    zip.start_file("OEBPS/toc.ncx", opts).unwrap();
    zip.write_all(
        br#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><docTitle><text>P</text></docTitle><navMap><navPoint id="n0" playOrder="0"><navLabel><text>Ch</text></navLabel><content src="Text/ch1.xhtml"/></navPoint></navMap></ncx>"#,
    )
    .unwrap();
    zip.finish().unwrap();
}

#[test]
fn inline_images_load_with_dims() {
    use image::codecs::png::PngEncoder;
    use image::ImageEncoder;
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            &vec![180u8; 300 * 100 * 4],
            300,
            100,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("img.epub");
    make_epub_with_image(&p, &png);
    let (_, chapters) = epub_utils::load_book(&p).expect("load");
    assert_eq!(chapters.len(), 1);
    let kinds: Vec<&str> = chapters[0]
        .blocks
        .iter()
        .map(|b| match b {
            marginalia::epub_utils::Block::Heading(_, _) => "h",
            marginalia::epub_utils::Block::Paragraph(_) => "p",
            marginalia::epub_utils::Block::Quote(_) => "q",
            marginalia::epub_utils::Block::ListItem(_) => "li",
            marginalia::epub_utils::Block::Pre(_) => "pre",
            marginalia::epub_utils::Block::Image { .. } => "img",
        })
        .collect();
    // Direct ../ path and bare-filename fallback both resolve; the missing,
    // remote, and unlisted images produce nothing.
    assert_eq!(kinds, vec!["p", "img", "img", "p"]);
    for b in &chapters[0].blocks {
        if let marginalia::epub_utils::Block::Image { w, h, bytes, .. } = b {
            assert_eq!((*w, *h), (300, 100));
            assert_eq!(bytes, &png);
        }
    }
    let alts: Vec<&str> = chapters[0]
        .blocks
        .iter()
        .filter_map(|b| match b {
            marginalia::epub_utils::Block::Image { alt, .. } => Some(alt.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(alts, vec!["A pic", "Fallback"]);
}

#[test]
fn ao3_preface_link_becomes_tracked() {
    let libdir = tempfile::tempdir().unwrap();
    let lib = Library::open(libdir.path().join("lib")).expect("open lib");

    let srcdir = tempfile::tempdir().unwrap();
    let epub = srcdir.path().join("ao3.epub");
    make_epub_ao3(&epub, "AO3 Story", "Some Author", 98765);

    // No URL identifier in the file, but the preface link is picked up and
    // canonicalized to https.
    let outcome = lib.add_file(&epub).expect("add");
    let book = match outcome {
        marginalia::library::AddOutcome::Added(b) => b,
        marginalia::library::AddOutcome::Duplicate(b) => b,
    };
    assert_eq!(
        book.fanfic_url.as_deref(),
        Some("https://archiveofourown.org/works/98765")
    );
    assert_eq!(lib.tracked().unwrap().len(), 1);
}

#[test]
fn backfill_links_untracked_books() {
    let libdir = tempfile::tempdir().unwrap();
    let lib = Library::open(libdir.path().join("lib")).expect("open lib");

    let srcdir = tempfile::tempdir().unwrap();
    // Untracked at import: preface scan runs at import too, so simulate a
    // legacy row by clearing the URL afterwards.
    let epub = srcdir.path().join("legacy.epub");
    make_epub_ao3(&epub, "Legacy Story", "Old Author", 11223);
    let outcome = lib.add_file(&epub).expect("add");
    let book = match outcome {
        marginalia::library::AddOutcome::Added(b) => b,
        marginalia::library::AddOutcome::Duplicate(b) => b,
    };
    let conn = rusqlite::Connection::open(libdir.path().join("lib/index.db")).unwrap();
    conn.execute("UPDATE books SET fanfic_url = '' WHERE id = ?1", [book.id])
        .unwrap();
    drop(conn);
    assert!(lib.tracked().unwrap().is_empty());

    let filled = lib.backfill_fanfic_urls().expect("backfill");
    assert_eq!(filled, vec![book.id]);
    let tracked = lib.tracked().unwrap();
    assert_eq!(tracked.len(), 1);
    assert_eq!(
        tracked[0].fanfic_url.as_deref(),
        Some("https://archiveofourown.org/works/11223")
    );

    // Second pass finds nothing new.
    assert!(lib.backfill_fanfic_urls().expect("backfill").is_empty());
}

/// EPUB3 nav-only fixture: no NCX, TOC comes from the nav document.
fn make_epub3_nav(path: &Path) {
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
    zip.write_all(
        br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    )
    .unwrap();
    let opf = r#"<?xml version="1.0"?><package version="3.0" xmlns="http://www.idpf.org/2007/opf" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Nav Book</dc:title><dc:creator>Nav Author</dc:creator><dc:language>en</dc:language></metadata><manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/><item id="c2" href="c2.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#;
    zip.start_file("OEBPS/content.opf", opts).unwrap();
    zip.write_all(opf.as_bytes()).unwrap();
    zip.start_file("OEBPS/c1.xhtml", opts).unwrap();
    zip.write_all(br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><h1>First</h1><p>Body.</p></body></html>"#)
        .unwrap();
    zip.start_file("OEBPS/c2.xhtml", opts).unwrap();
    zip.write_all(br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Second</h1><p>Body.</p></body></html>"#)
        .unwrap();
    zip.start_file("OEBPS/nav.xhtml", opts).unwrap();
    zip.write_all(br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="c1.xhtml">First</a></li><li><a href="c2.xhtml#sec">Second</a></li></ol></nav></body></html>"#)
        .unwrap();
    zip.finish().unwrap();
}

#[test]
fn epub3_nav_toc_without_ncx() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("nav.epub");
    make_epub3_nav(&p);
    let (toc, chapters) = epub_utils::load_book(&p).expect("load");
    assert_eq!(chapters.len(), 2);
    assert_eq!(toc.len(), 2);
    assert_eq!(toc[0].chapter, Some(0));
    assert_eq!(toc[1].chapter, Some(1));
    assert_eq!(chapters[0].label, "First");
    assert_eq!(chapters[1].label, "Second");
}

#[test]
fn epub3_cover_image_resolves() {
    use image::codecs::png::PngEncoder;
    use image::ImageEncoder;
    use std::io::Write;
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            &vec![180u8; 16 * 16 * 4],
            16,
            16,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("cover.epub");
    {
        let file = std::fs::File::create(&p).unwrap();
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
        let opf = r#"<?xml version="1.0"?><package version="3.0" xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>C</dc:title><dc:creator>A</dc:creator></metadata><manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/><item id="cov" href="cover.png" media-type="image/png" properties="cover-image"/></manifest><spine><itemref idref="c1"/></spine></package>"#;
        zip.start_file("OEBPS/content.opf", opts).unwrap();
        zip.write_all(opf.as_bytes()).unwrap();
        zip.start_file("OEBPS/c1.xhtml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><p>Hi.</p></body></html>"#).unwrap();
        zip.start_file("OEBPS/cover.png", opts).unwrap();
        zip.write_all(&png).unwrap();
        zip.finish().unwrap();
    }
    let mut doc = marginalia::epub_core::EpubDoc::new(&p).expect("open");
    let (bytes, mime) = doc.get_cover().expect("cover");
    assert_eq!(bytes, png);
    assert!(mime.contains("png"));
}
