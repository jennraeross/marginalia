use chrono::Utc;
use epub::doc::EpubDoc;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Clone)]
pub struct Book {
    pub id: i64,
    pub hash: String,
    pub short_id: String,
    pub title: String,
    pub authors: String,
    pub series: String,
    pub series_index: Option<f32>,
    pub publisher: String,
    pub published: String,
    pub language: String,
    pub subjects: String,
    pub description: String,
    pub fanfic_url: Option<String>,
    pub file_path: String,
    pub file_size: i64,
    pub added_at: i64,
    pub opened_at: i64,
    pub progress_chapter: i64,
    pub progress_page: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Recent,
    Author,
    Title,
    Series,
    Published,
    Added,
    Size,
    Progress,
}

impl SortKey {
    pub fn all() -> &'static [(&'static str, SortKey)] {
        &[
            ("Recent", SortKey::Recent),
            ("Author", SortKey::Author),
            ("Title", SortKey::Title),
            ("Series", SortKey::Series),
            ("Published", SortKey::Published),
            ("Added", SortKey::Added),
            ("Size", SortKey::Size),
            ("Progress", SortKey::Progress),
        ]
    }
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Recent => "Recent",
            SortKey::Author => "Author",
            SortKey::Title => "Title",
            SortKey::Series => "Series",
            SortKey::Published => "Published",
            SortKey::Added => "Added",
            SortKey::Size => "Size",
            SortKey::Progress => "Progress",
        }
    }
}

#[derive(Debug, Clone)]
pub enum AddOutcome {
    Added(Book),
    Duplicate(Book),
}

/// Reader display settings, persisted in the `settings` table.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySettings {
    pub font_family: String,
    pub font_size: f32,
    pub font_weight: f32,
    pub max_width: f32,
    pub column_mode: String,
    pub theme: String,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            font_family: "Bitter".to_string(),
            font_size: 18.0,
            font_weight: 450.0,
            max_width: 900.0,
            column_mode: "Auto".to_string(),
            theme: "default-dark".to_string(),
        }
    }
}

pub struct Library {
    pub root: PathBuf,
    pub db_path: PathBuf,
}

impl Library {
    pub fn default_root() -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join("Library/Application Support/marginalia");
            }
        }
        if let Some(proj) = directories::ProjectDirs::from("", "", "marginalia") {
            return proj.data_dir().to_path_buf();
        }
        PathBuf::from(".marginalia")
    }

    pub fn open(root: PathBuf) -> AnyhowResult<Self> {
        let db_path = root.join("index.db");
        let lib = Self { root, db_path };
        lib.ensure_layout()?;
        lib.init_db()?;
        Ok(lib)
    }

    fn ensure_layout(&self) -> AnyhowResult {
        fs::create_dir_all(self.root.join("books"))?;
        #[cfg(unix)]
        {
            let perm = fs::Permissions::from_mode(0o700);
            let _ = fs::set_permissions(&self.root, perm.clone());
            let _ = fs::set_permissions(self.root.join("books"), perm);
        }
        Ok(())
    }

    fn connect(&self) -> rusqlite::Result<Connection> {
        Connection::open(&self.db_path)
    }

    fn init_db(&self) -> AnyhowResult {
        let conn = self.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS books (
                id INTEGER PRIMARY KEY,
                hash TEXT UNIQUE NOT NULL,
                short_id TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                authors TEXT NOT NULL DEFAULT '',
                series TEXT NOT NULL DEFAULT '',
                series_index REAL,
                publisher TEXT NOT NULL DEFAULT '',
                published TEXT NOT NULL DEFAULT '',
                language TEXT NOT NULL DEFAULT '',
                subjects TEXT NOT NULL DEFAULT '',
                description TEXT NOT NULL DEFAULT '',
                fanfic_url TEXT,
                file_path TEXT NOT NULL,
                file_size INTEGER NOT NULL DEFAULT 0,
                added_at INTEGER NOT NULL,
                opened_at INTEGER NOT NULL,
                progress_chapter INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_books_hash ON books(hash);
            CREATE INDEX IF NOT EXISTS idx_books_title ON books(title);
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        // Migration for libraries created before page-level progress existed.
        let has_page: bool = conn
            .prepare("SELECT progress_page FROM books LIMIT 0")
            .is_ok();
        if !has_page {
            conn.execute_batch(
                "ALTER TABLE books ADD COLUMN progress_page INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(&self.db_path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn books_dir(&self) -> PathBuf {
        self.root.join("books")
    }

    pub fn abs_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn add_file(&self, src: &Path) -> AnyhowResult<AddOutcome> {
        let hash = sha256_file(src)?;
        if let Some(existing) = self.find_by_hash(&hash)? {
            return Ok(AddOutcome::Duplicate(existing));
        }
        let meta = parse_epub_meta(src)?;
        let now = Utc::now().timestamp();
        let author_dir = sanitize_component(&first_author(&meta.authors));
        let title_dir = sanitize_component(&meta.title);
        let mut dest_dir = self.books_dir().join(&author_dir).join(&title_dir);
        if dest_dir.exists() {
            let short = &hash[..12];
            dest_dir = self
                .books_dir()
                .join(&author_dir)
                .join(format!("{title_dir} {short}"));
        }
        fs::create_dir_all(&dest_dir)?;
        let dest = dest_dir.join("book.epub");

        // Verified copy via temp file.
        let tmp = dest_dir.join("book.epub.tmp");
        fs::copy(src, &tmp)?;
        let tmp_hash = sha256_file(&tmp)?;
        if tmp_hash != hash {
            let _ = fs::remove_file(&tmp);
            anyhow_bail!("hash mismatch after copy");
        }
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        fs::rename(&tmp, &dest)?;
        #[cfg(unix)]
        {
            let perm = fs::Permissions::from_mode(0o600);
            let _ = fs::set_permissions(&dest, perm);
        }

        // Cover.
        if let Some((bytes, mime)) = extract_cover(src) {
            let ext = mime_to_ext(&mime);
            let cover_path = dest_dir.join(format!("cover.{ext}"));
            let _ = fs::write(&cover_path, &bytes);
            #[cfg(unix)]
            {
                let _ = fs::set_permissions(&cover_path, fs::Permissions::from_mode(0o600));
            }
        }

        let rel = dest
            .strip_prefix(&self.root)
            .unwrap_or(&dest)
            .to_string_lossy()
            .to_string();
        let size = fs::metadata(&dest).map(|m| m.len() as i64).unwrap_or(0);
        let short_id = hash[..12].to_string();

        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO books (hash, short_id, title, authors, series, series_index, publisher, published, language, subjects, description, fanfic_url, file_path, file_size, added_at, opened_at, progress_chapter)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,0)",
            params![
                hash,
                short_id,
                meta.title,
                meta.authors.join(", "),
                meta.series,
                meta.series_index,
                meta.publisher,
                meta.published,
                meta.language,
                meta.subjects.join(", "),
                meta.description,
                meta.fanfic_url,
                rel,
                size,
                now,
                now,
            ],
        )?;
        let id = conn.last_insert_rowid();
        let book = self.get_by_id(id)?.expect("just inserted");
        Ok(AddOutcome::Added(book))
    }

    /// Replace the stored EPUB for `book_id` with `src` (used after FanFicFare update).
    /// Preserves progress chapter (clamped) and re-reads metadata.
    pub fn replace_file(&self, book_id: i64, src: &Path) -> AnyhowResult<Book> {
        let book = self.get_by_id(book_id)?.expect("book exists");
        let hash = sha256_file(src)?;
        let meta = parse_epub_meta(src)?;
        let dest = self.abs_path(&book.file_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = dest.with_extension("epub.tmp");
        fs::copy(src, &tmp)?;
        let tmp_hash = sha256_file(&tmp)?;
        if tmp_hash != hash {
            let _ = fs::remove_file(&tmp);
            anyhow_bail!("hash mismatch after copy");
        }
        fs::rename(&tmp, &dest)?;
        let size = fs::metadata(&dest).map(|m| m.len() as i64).unwrap_or(0);
        if let Some((bytes, mime)) = extract_cover(src) {
            if let Some(parent) = dest.parent() {
                let cover_path = parent.join(format!("cover.{}", mime_to_ext(&mime)));
                let _ = fs::write(&cover_path, &bytes);
            }
        }
        let conn = self.connect()?;
        conn.execute(
            "UPDATE books SET hash=?1, short_id=?2, title=?3, authors=?4, series=?5, series_index=?6,
             publisher=?7, published=?8, language=?9, subjects=?10, description=?11, fanfic_url=?12,
             file_size=?13, opened_at=?14 WHERE id=?15",
            params![
                hash,
                hash[..12].to_string(),
                meta.title,
                meta.authors.join(", "),
                meta.series,
                meta.series_index,
                meta.publisher,
                meta.published,
                meta.language,
                meta.subjects.join(", "),
                meta.description,
                meta.fanfic_url.or(book.fanfic_url.clone()),
                size,
                Utc::now().timestamp(),
                book_id,
            ],
        )?;
        // Clamp progress in case the book shrank.
        let num = crate::epub_utils::chapter_count(&dest).unwrap_or(1).max(1) as i64;
        let clamped = book.progress_chapter.min(num - 1).max(0);
        conn.execute(
            "UPDATE books SET progress_chapter=?1 WHERE id=?2",
            params![clamped, book_id],
        )?;
        Ok(self.get_by_id(book_id)?.expect("updated"))
    }

    pub fn find_by_hash(&self, hash: &str) -> AnyhowResult<Option<Book>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare("SELECT * FROM books WHERE hash = ?1")?;
        let mut rows = stmt.query_map(params![hash], row_to_book)?;
        if let Some(r) = rows.next() {
            return Ok(Some(r?));
        }
        Ok(None)
    }

    pub fn get_by_id(&self, id: i64) -> AnyhowResult<Option<Book>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare("SELECT * FROM books WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], row_to_book)?;
        if let Some(r) = rows.next() {
            return Ok(Some(r?));
        }
        Ok(None)
    }

    pub fn list(&self, query: &str, sort: SortKey) -> AnyhowResult<Vec<Book>> {
        let conn = self.connect()?;
        let order = match sort {
            SortKey::Recent => "max(added_at, opened_at) DESC",
            SortKey::Author => "authors COLLATE NOCASE, title COLLATE NOCASE",
            SortKey::Title => "title COLLATE NOCASE",
            SortKey::Series => "series COLLATE NOCASE, series_index, published",
            SortKey::Published => "published DESC",
            SortKey::Added => "added_at DESC",
            SortKey::Size => "file_size DESC",
            SortKey::Progress => "progress_chapter DESC, max(added_at, opened_at) DESC",
        };
        let q = query.trim().to_lowercase();
        let sql = format!("SELECT * FROM books ORDER BY {order}");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_book)?;
        let mut out = Vec::new();
        for r in rows {
            let b = r?;
            if q.is_empty() {
                out.push(b);
                continue;
            }
            let hay = format!(
                "{} {} {} {} {} {} {}",
                b.title, b.authors, b.series, b.subjects, b.description, b.short_id, b.hash
            )
            .to_lowercase();
            if hay.contains(&q) {
                out.push(b);
            }
        }
        Ok(out)
    }

    pub fn tracked(&self) -> AnyhowResult<Vec<Book>> {
        let conn = self.connect()?;
        let mut stmt =
            conn.prepare("SELECT * FROM books WHERE fanfic_url IS NOT NULL AND fanfic_url != ''")?;
        let rows = stmt.query_map([], row_to_book)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn touch_opened(&self, id: i64) -> AnyhowResult {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE books SET opened_at=?1 WHERE id=?2",
            params![Utc::now().timestamp(), id],
        )?;
        Ok(())
    }

    pub fn save_progress(&self, id: i64, chapter: i64, page: i64) -> AnyhowResult {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE books SET progress_chapter=?1, progress_page=?2, opened_at=?3 WHERE id=?4",
            params![chapter, page, Utc::now().timestamp(), id],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> AnyhowResult<Option<String>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |row| row.get::<_, String>(0))?;
        if let Some(r) = rows.next() {
            return Ok(Some(r?));
        }
        Ok(None)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AnyhowResult {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn setting_or(&self, key: &str, fallback: &str) -> String {
        self.get_setting(key)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| fallback.to_string())
    }

    fn setting_or_parse<T: std::str::FromStr>(&self, key: &str, fallback: T) -> T {
        self.get_setting(key)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    }

    /// Display settings with per-field fallbacks, so a corrupt or partial row
    /// can never break startup. Range checks happen in the app layer.
    /// Pre-theme libraries stored a `ui.dark_theme` bool; that maps onto the
    /// matching Default theme when no `ui.theme` row exists yet.
    pub fn get_display_settings(&self) -> DisplaySettings {
        let d = DisplaySettings::default();
        let theme = self.setting_or("ui.theme", "");
        let theme = if theme.is_empty() {
            let dark = self.setting_or_parse("ui.dark_theme", d.theme == "default-dark");
            if dark {
                "default-dark"
            } else {
                "default-light"
            }
            .to_string()
        } else {
            theme
        };
        DisplaySettings {
            font_family: self.setting_or("reader.font_family", &d.font_family),
            font_size: self.setting_or_parse("reader.font_size", d.font_size),
            font_weight: self.setting_or_parse("reader.font_weight", d.font_weight),
            max_width: self.setting_or_parse("reader.max_width", d.max_width),
            column_mode: self.setting_or("reader.column_mode", &d.column_mode),
            theme,
        }
    }

    pub fn save_display_settings(&self, s: &DisplaySettings) -> AnyhowResult {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let pairs = [
            ("reader.font_family", s.font_family.clone()),
            ("reader.font_size", s.font_size.to_string()),
            ("reader.font_weight", s.font_weight.to_string()),
            ("reader.max_width", s.max_width.to_string()),
            ("reader.column_mode", s.column_mode.clone()),
            ("ui.theme", s.theme.clone()),
        ];
        for (k, v) in pairs {
            tx.execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![k, v],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

pub type AnyhowResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
macro_rules! anyhow_bail {
    ($msg:expr) => {
        return Err($msg.into())
    };
}
pub(crate) use anyhow_bail;

fn row_to_book(row: &rusqlite::Row) -> rusqlite::Result<Book> {
    Ok(Book {
        id: row.get("id")?,
        hash: row.get("hash")?,
        short_id: row.get("short_id")?,
        title: row.get("title")?,
        authors: row.get("authors")?,
        series: row.get("series")?,
        series_index: row.get("series_index")?,
        publisher: row.get("publisher")?,
        published: row.get("published")?,
        language: row.get("language")?,
        subjects: row.get("subjects")?,
        description: row.get("description")?,
        fanfic_url: row.get("fanfic_url")?,
        file_path: row.get("file_path")?,
        file_size: row.get("file_size")?,
        added_at: row.get("added_at")?,
        opened_at: row.get("opened_at")?,
        progress_chapter: row.get("progress_chapter")?,
        progress_page: row.get("progress_page").unwrap_or(0),
    })
}

pub fn sha256_file(path: &Path) -> AnyhowResult<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn sanitize_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string();
    if out.is_empty() {
        out = "Unknown".to_string();
    }
    if out.len() > 80 {
        out.truncate(80);
    }
    out
}

fn first_author(authors: &[String]) -> String {
    authors.first().cloned().unwrap_or_else(|| "Unknown".into())
}

fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        m if m.contains("png") => "png",
        m if m.contains("gif") => "gif",
        m if m.contains("webp") => "webp",
        _ => "jpg",
    }
}

/// Human-readable file size, e.g. `1.7 MiB`, `523 KiB`, `12 B`.
pub fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes.max(0) as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", size as i64, UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Calendar date for a Unix timestamp, e.g. `2026-11-13` in local time.
pub fn human_date(timestamp: i64) -> String {
    use chrono::{DateTime, Local};
    DateTime::from_timestamp(timestamp, 0)
        .map(|utc: DateTime<chrono::Utc>| utc.with_timezone(&Local).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

pub struct EpubMeta {
    pub title: String,
    pub authors: Vec<String>,
    pub series: String,
    pub series_index: Option<f32>,
    pub publisher: String,
    pub published: String,
    pub language: String,
    pub subjects: Vec<String>,
    pub description: String,
    pub fanfic_url: Option<String>,
}

pub fn parse_epub_meta(path: &Path) -> AnyhowResult<EpubMeta> {
    let doc = EpubDoc::new(path).map_err(|e| format!("invalid epub: {e}"))?;
    let mget = |names: &[&str]| -> Vec<String> {
        doc.metadata
            .iter()
            .filter(|m| {
                let p = m.property.to_lowercase();
                names.iter().any(|n| p.contains(n))
            })
            .map(|m| m.value.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect()
    };
    let one = |names: &[&str]| -> String { mget(names).first().cloned().unwrap_or_default() };

    let title = doc
        .get_title()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| one(&["title"]));
    let title = if title.is_empty() {
        "Unknown".to_string()
    } else {
        title
    };

    let mut authors = mget(&["creator", "author"]);
    if authors.is_empty() {
        authors = mget(&["contributor"]);
    }

    let series = one(&["calibre:series", "belongs-to-collection", "collection"]);
    let series_index = one(&["calibre:series_index", "group-position", "series_index"])
        .parse::<f32>()
        .ok();
    let publisher = one(&["publisher"]);
    let published = one(&["date"]);
    let language = one(&["language"]);
    let subjects = mget(&["subject"]);
    let identifiers = mget(&["identifier"]);
    let fanfic_url = identifiers
        .iter()
        .chain(mget(&["source"]).iter())
        .find(|v| v.starts_with("http"))
        .cloned();
    let raw_desc = one(&["description"]);
    let description = crate::epub_utils::strip_html(&raw_desc);

    Ok(EpubMeta {
        title,
        authors,
        series,
        series_index,
        publisher,
        published,
        language,
        subjects,
        description,
        fanfic_url,
    })
}

fn extract_cover(path: &Path) -> Option<(Vec<u8>, String)> {
    let mut doc = EpubDoc::new(path).ok()?;
    doc.get_cover()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(523_000), "510.7 KiB");
        assert_eq!(human_size(1_782_000), "1.7 MiB");
        assert_eq!(human_size(-5), "0 B");
    }

    #[test]
    fn human_date_formats() {
        // 2026-11-13 in UTC; local rendering keeps the calendar date shape.
        let s = human_date(1_794_556_800);
        assert_eq!(s.len(), 10);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
        assert!(human_date(-1).is_empty() || human_date(-1).len() == 10);
    }

    #[test]
    fn sort_key_round_trip() {
        for (label, key) in SortKey::all() {
            assert_eq!(key.label(), *label);
        }
        // New sortable columns exist for the details view.
        assert!(SortKey::all().iter().any(|(_, k)| *k == SortKey::Size));
        assert!(SortKey::all().iter().any(|(_, k)| *k == SortKey::Progress));
    }
}
