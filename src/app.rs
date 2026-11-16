use crate::epub_utils::{self, plain_spans, spans_plain, Block, Chapter, TocEntry};
use crate::fanficfare;
use crate::fonts;
use crate::library::{AddOutcome, Book, Library, SortKey};
use crate::speech::{blocks_to_speech, SpeechEngine};
use crate::themes::ThemeId;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};

enum JobEvent {
    Message(String),
    Refresh,
    ReaderReload(i64),
    CheckDone(Vec<(i64, fanficfare::CheckState)>),
    Done(String),
    Error(String),
}

struct JobState {
    busy: bool,
    log: VecDeque<String>,
}

impl JobState {
    fn push(&mut self, s: impl Into<String>) {
        let s = s.into();
        if self.log.len() > 200 {
            self.log.pop_front();
        }
        self.log.push_back(s);
    }
    fn last(&self) -> &str {
        self.log.back().map(|s| s.as_str()).unwrap_or("")
    }
}

#[derive(PartialEq)]
enum View {
    Library,
    Reader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnMode {
    Auto,
    One,
    Two,
}

impl ColumnMode {
    fn label(self) -> &'static str {
        match self {
            ColumnMode::Auto => "Auto",
            ColumnMode::One => "One",
            ColumnMode::Two => "Two",
        }
    }

    fn all() -> &'static [ColumnMode] {
        &[ColumnMode::Auto, ColumnMode::One, ColumnMode::Two]
    }

    fn from_label(s: &str) -> Option<ColumnMode> {
        match s {
            "Auto" => Some(ColumnMode::Auto),
            "One" => Some(ColumnMode::One),
            "Two" => Some(ColumnMode::Two),
            _ => None,
        }
    }
}

/// Keys that turn pages. They (and the Prev/Next buttons) deliberately don't
/// reset the chrome fade timer, so steady reading fades the bars.
fn is_page_turn_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ArrowLeft
            | egui::Key::ArrowRight
            | egui::Key::PageUp
            | egui::Key::PageDown
            | egui::Key::Space
    )
}

/// A book can be update-checked when it carries a FanFicFare source URL
/// (set at import from the EPUB metadata, preserved across updates).
fn is_updatable(b: &Book) -> bool {
    b.fanfic_url.as_ref().is_some_and(|u| !u.trim().is_empty())
}

/// Char-index slice of `s` (`start..end` in chars, clamped). Never panics on
/// multi-byte boundaries.
fn slice_chars(s: &str, start: usize, end: usize) -> String {
    let (lo, hi) = (start.min(end), start.max(end));
    s.chars().skip(lo).take(hi.saturating_sub(lo)).collect()
}

/// One page of a chapter: `flow[start..end]`, optionally split across two
/// columns at `split` (an index into `flow`; `None` renders full width).
#[derive(Debug, Clone)]
struct PageDef {
    start: usize,
    end: usize,
    split: Option<usize>,
}

/// One text block's on-screen rect for reader text selection hit-testing.
/// `flow_idx` indexes into `flow`; `col` is 0 (left/single) or 1 (right).
#[derive(Debug, Clone)]
struct SelBlock {
    flow_idx: usize,
    rect: egui::Rect,
    col: u8,
}

/// An enlarged inline image shown in a modal overlay.
#[derive(Debug, Clone)]
struct Lightbox {
    /// Texture-cache key (`image_key` of the block's content hash).
    tex_key: String,
    native_w: u32,
    native_h: u32,
}

/// Gutter between the two reader columns.
const COLUMN_GAP: f32 = 48.0;
/// Live-resize settle delay: while the window geometry keeps changing faster
/// than this, the reader keeps showing the last laid-out pages and reflows
/// once the resize settles, instead of reshaping text every frame.
const RESIZE_SETTLE_MS: u64 = 150;
/// Minimum comfortable width of a single reader column. `Auto` mode shows
/// two columns only when the content fits two of these plus the gutter.
const MIN_COLUMN_WIDTH: f32 = 320.0;

/// Whether the details pane goes in the right column (wide viewports) or the
/// bottom panel (narrow/portrait viewports).
fn details_on_right(available: egui::Vec2) -> bool {
    available.x >= available.y
}

/// Cache key for the paginated layout. Any change invalidates the pages.
#[derive(Debug, Clone, PartialEq)]
struct PagesKey {
    book: i64,
    chapter: usize,
    col_w: u32,
    page_h: u32,
    size_bits: u32,
    weight_bits: u32,
    family: String,
    mode: ColumnMode,
}

pub struct MarginaliaApp {
    lib: Library,
    books: Vec<Book>,
    /// Library-wide update eligibility, refreshed with the book list.
    tracked_count: usize,
    library_total: usize,
    /// Session-only update-check results by book id.
    check_state: HashMap<i64, fanficfare::CheckState>,
    query: String,
    /// Stable widget id for the library search box (keyboard-nav guard).
    search_id: egui::Id,
    sort: SortKey,
    sort_reverse: bool,
    selected: Option<i64>,
    /// Select mode for batch operations: checked book ids (stable across
    /// sort/search/refresh). Cleared when leaving select mode.
    select_mode: bool,
    checked: std::collections::HashSet<i64>,
    /// Pending delete confirmation: (id, title) pairs awaiting Confirm.
    delete_confirm: Option<Vec<(i64, String)>>,
    view: View,
    reader_book: Option<Book>,
    toc: Vec<TocEntry>,
    chapters: Vec<Chapter>,
    current: usize,
    page: usize,
    pages: Vec<PageDef>,
    flow: Vec<Block>,
    pages_key: Option<PagesKey>,
    last_content_w: f32,
    last_page_h: f32,
    toc_open: bool,
    font_size: f32,
    font_weight: f32,
    reader_font: String,
    column_mode: ColumnMode,
    max_width: f32,
    saved_display: crate::library::DisplaySettings,
    /// Reader chrome fade state: last user activity and current bar opacity.
    last_activity: std::time::Instant,
    last_geom: Option<(u32, u32)>,
    last_geom_change: std::time::Instant,
    built_two_col: bool,
    chrome_opacity: f32,
    /// True while the OS cursor is hidden for faded reader chrome.
    cursor_hidden: bool,
    /// Enlarged inline image overlay, if open.
    lightbox: Option<Lightbox>,
    /// True while the lightbox was opened this frame: skips the click-outside
    /// check so the opening click can't instantly dismiss it.
    lightbox_new: bool,
    /// Cross-column drag selection: anchor/active as (flow index, char offset
    /// in layout text). Cleared on page/chapter turn and reflow.
    sel_anchor: Option<(usize, usize)>,
    sel_active: Option<(usize, usize)>,
    /// True while the primary button is held down starting on reader text.
    sel_dragging: bool,
    /// On-screen rects of the current page's text blocks (previous frame).
    sel_rects: Vec<SelBlock>,
    /// Column width the cached `sel_rects` were laid out with.
    sel_col_w: f32,
    textures: HashMap<String, egui::TextureHandle>,
    tx: Sender<JobEvent>,
    rx: Receiver<JobEvent>,
    jobs: JobState,
    speech: SpeechEngine,
    reading_aloud: bool,
    speak_pending: bool,
    show_download: bool,
    download_url: String,
    show_display: bool,
    show_help: bool,
    theme: ThemeId,
    error: Option<String>,
}

impl MarginaliaApp {
    pub fn new(lib: Library, cc: &eframe::CreationContext) -> Self {
        let (tx, rx) = mpsc::channel();
        fonts::install(&cc.egui_ctx);
        // Restore persisted display settings, validating each field against
        // the bundled options and control ranges.
        let stored = lib.get_display_settings();
        let reader_font = if fonts::READER_FONTS
            .iter()
            .any(|(n, _)| *n == stored.font_family)
        {
            stored.font_family.clone()
        } else {
            fonts::default_family().to_string()
        };
        let column_mode = ColumnMode::from_label(&stored.column_mode).unwrap_or(ColumnMode::Auto);
        let theme = ThemeId::from_id(&stored.theme).unwrap_or(ThemeId::DefaultDark);
        let saved_display = crate::library::DisplaySettings {
            font_family: reader_font.clone(),
            font_size: stored.font_size.clamp(12.0, 26.0),
            font_weight: stored.font_weight.clamp(300.0, 700.0),
            max_width: stored.max_width.clamp(420.0, 1100.0),
            column_mode: column_mode.label().to_string(),
            theme: theme.id().to_string(),
        };
        let mut app = Self {
            lib,
            books: vec![],
            tracked_count: 0,
            library_total: 0,
            check_state: HashMap::new(),
            query: String::new(),
            search_id: egui::Id::unique("library-search"),
            sort: SortKey::Recent,
            sort_reverse: false,
            selected: None,
            select_mode: false,
            checked: std::collections::HashSet::new(),
            delete_confirm: None,
            view: View::Library,
            reader_book: None,
            toc: vec![],
            chapters: vec![],
            current: 0,
            page: 0,
            pages: vec![],
            flow: vec![],
            pages_key: None,
            last_content_w: 0.0,
            last_page_h: 0.0,
            toc_open: false,
            font_size: saved_display.font_size,
            font_weight: saved_display.font_weight,
            reader_font: saved_display.font_family.clone(),
            column_mode,
            max_width: saved_display.max_width,
            saved_display,
            last_activity: std::time::Instant::now(),
            last_geom: None,
            last_geom_change: std::time::Instant::now(),
            built_two_col: false,
            chrome_opacity: 1.0,
            cursor_hidden: false,
            lightbox: None,
            lightbox_new: false,
            sel_anchor: None,
            sel_active: None,
            sel_dragging: false,
            sel_rects: Vec::new(),
            sel_col_w: 0.0,
            textures: HashMap::new(),
            tx,
            rx,
            jobs: JobState {
                busy: false,
                log: VecDeque::new(),
            },
            speech: SpeechEngine::new(),
            reading_aloud: false,
            speak_pending: false,
            show_download: false,
            download_url: String::new(),
            show_display: false,
            show_help: false,
            theme,
            error: None,
        };
        cc.egui_ctx.set_visuals(app.theme.visuals());
        app.refresh();
        app.maybe_backfill_fanfic_urls();
        app.start_silent_update_check();
        app
    }

    /// Link direct-AO3 downloads to their work URLs by scanning prefaces.
    /// One startup background pass over untracked books only; results flow
    /// back through the job channel and refresh the counts/markers.
    fn maybe_backfill_fanfic_urls(&mut self) {
        if self.library_total <= self.tracked_count {
            return;
        }
        self.jobs.busy = true;
        self.jobs.push("linking AO3 fics …".to_string());
        let tx = self.tx.clone();
        let root = self.lib.root.clone();
        std::thread::spawn(move || {
            let lib = match Library::open(root) {
                Ok(l) => l,
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(format!("open library: {e}")));
                    return;
                }
            };
            match lib.backfill_fanfic_urls() {
                Ok(filled) => {
                    if filled.is_empty() {
                        let _ = tx.send(JobEvent::Done("fic links up to date".to_string()));
                    } else {
                        let _ = tx.send(JobEvent::Done(format!(
                            "linked {} fic{} to AO3",
                            filled.len(),
                            if filled.len() == 1 { "" } else { "s" }
                        )));
                    }
                }
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(format!("fic link scan failed: {e}")));
                }
            }
        });
    }

    /// Snapshot of the current display controls for persistence.
    fn current_display(&self) -> crate::library::DisplaySettings {
        crate::library::DisplaySettings {
            font_family: self.reader_font.clone(),
            font_size: self.font_size,
            font_weight: self.font_weight,
            max_width: self.max_width,
            column_mode: self.column_mode.label().to_string(),
            theme: self.theme.id().to_string(),
        }
    }

    /// Persist display settings when anything changed since the last flush.
    fn flush_display_settings(&mut self) {
        let cur = self.current_display();
        if cur != self.saved_display && self.lib.save_display_settings(&cur).is_ok() {
            self.saved_display = cur;
        }
    }

    fn refresh(&mut self) {
        match self.lib.list(&self.query, self.sort) {
            Ok(mut books) => {
                if self.sort_reverse {
                    books.reverse();
                }
                if let Some(sel) = self.selected {
                    if !books.iter().any(|b| b.id == sel) {
                        self.selected = books.first().map(|b| b.id);
                    }
                } else {
                    self.selected = books.first().map(|b| b.id);
                }
                self.books = books;
            }
            Err(e) => self.error = Some(format!("list failed: {e}")),
        }
        // Library-wide update eligibility for the top bar counts. Kept
        // separate from the (possibly query-filtered) book list.
        self.tracked_count = self.lib.tracked().map(|v| v.len()).unwrap_or(0);
        self.library_total = if self.query.trim().is_empty() {
            self.books.len()
        } else {
            self.lib
                .list("", SortKey::Recent)
                .map(|v| v.len())
                .unwrap_or(self.books.len())
        };
    }

    fn selected_book(&self) -> Option<&Book> {
        self.selected
            .and_then(|id| self.books.iter().find(|b| b.id == id))
    }

    fn poll_jobs(&mut self) {
        let mut dirty = false;
        let mut reload_reader: Option<i64> = None;
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                JobEvent::Message(m) => self.jobs.push(m),
                JobEvent::Refresh => dirty = true,
                JobEvent::ReaderReload(id) => {
                    dirty = true;
                    reload_reader = Some(id);
                }
                JobEvent::CheckDone(results) => {
                    for (id, state) in results {
                        self.check_state.insert(id, state);
                    }
                }
                JobEvent::Done(m) => {
                    self.jobs.push(m);
                    self.jobs.busy = false;
                    dirty = true;
                }
                JobEvent::Error(e) => {
                    self.jobs.push(format!("error: {e}"));
                    self.error = Some(e);
                    self.jobs.busy = false;
                    dirty = true;
                }
            }
        }
        if dirty {
            self.refresh();
        }
        if let Some(id) = reload_reader {
            if self.reader_book.as_ref().is_some_and(|b| b.id == id) {
                self.load_reader_chapters(id);
            }
        }
    }

    fn set_theme(&mut self, ctx: &egui::Context) {
        ctx.set_visuals(self.theme.visuals());
    }

    fn do_add_files(&mut self) {
        let files = rfd::FileDialog::new()
            .add_filter("EPUB", &["epub"])
            .pick_files();
        let Some(files) = files else { return };
        let mut added = 0;
        let mut dups = 0;
        for f in files {
            match self.lib.add_file(&f) {
                Ok(AddOutcome::Added(_)) => added += 1,
                Ok(AddOutcome::Duplicate(_)) => dups += 1,
                Err(e) => {
                    self.error = Some(format!("add {} failed: {e}", f.display()));
                }
            }
        }
        self.jobs
            .push(format!("add: {added} added, {dups} duplicates"));
        self.refresh();
    }

    fn open_reader(&mut self, id: i64) {
        self.stop_reading();
        let _ = self.lib.touch_opened(id);
        self.load_reader_chapters(id);
        self.view = View::Reader;
        // Chrome fully visible on entry; the fade timer restarts.
        self.last_activity = std::time::Instant::now();
        self.chrome_opacity = 1.0;
        self.refresh();
    }

    fn load_reader_chapters(&mut self, id: i64) {
        let Some(book) = self.lib.get_by_id(id).ok().flatten() else {
            self.error = Some("book not found".into());
            return;
        };
        let path = self.lib.abs_path(&book.file_path);
        match epub_utils::load_book(&path) {
            Ok((toc, chapters)) => {
                let start = (book.progress_chapter as usize).min(chapters.len().saturating_sub(1));
                let page = book.progress_page.max(0) as usize;
                self.reader_book = Some(book);
                self.toc = toc;
                self.chapters = chapters;
                self.current = start;
                // Pages are rebuilt on the next frame once geometry is known;
                // the saved page is clamped there.
                self.page = page;
                self.pages_key = None;
                self.clear_selection();
            }
            Err(e) => self.error = Some(format!("cannot read epub: {e}")),
        }
    }

    /// Show or hide the OS cursor, sending a viewport command only on
    /// transition. Cursor visibility follows the same fade target as the
    /// reader chrome, so any suppression of the chrome hide also suppresses
    /// the cursor hide.
    fn sync_cursor(&mut self, ctx: &egui::Context, should_hide: bool) {
        if should_hide != self.cursor_hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!should_hide));
            self.cursor_hidden = should_hide;
        }
    }

    fn close_reader(&mut self) {
        self.stop_reading();
        if let Some(b) = &self.reader_book {
            let _ = self
                .lib
                .save_progress(b.id, self.current as i64, self.page as i64);
        }
        self.reader_book = None;
        self.chapters.clear();
        self.toc.clear();
        self.pages.clear();
        self.flow.clear();
        self.pages_key = None;
        self.clear_selection();
        self.view = View::Library;
        self.refresh();
    }

    fn goto_chapter(&mut self, idx: usize) {
        if self.chapters.is_empty() {
            return;
        }
        let idx = idx.min(self.chapters.len() - 1);
        self.current = idx;
        self.page = 0;
        self.pages_key = None;
        self.clear_selection();
        if let Some(b) = &self.reader_book {
            let _ = self.lib.save_progress(b.id, idx as i64, 0);
        }
    }

    fn save_page(&mut self) {
        if let Some(b) = &self.reader_book {
            let _ = self
                .lib
                .save_progress(b.id, self.current as i64, self.page as i64);
        }
    }

    /// Clear the reader text selection (page/chapter turn, reflow, dismiss).
    fn clear_selection(&mut self) {
        self.sel_anchor = None;
        self.sel_active = None;
        self.sel_dragging = false;
    }

    /// Ordered selection endpoints, if a non-empty range is selected.
    fn ordered_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let (a, b) = (self.sel_anchor?, self.sel_active?);
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// Layout text of a block (what hit-testing and painting measure).
    /// List items include the rendered bullet prefix.
    fn layout_text_of(b: &Block) -> String {
        match b {
            Block::Heading(_, s) | Block::Paragraph(s) | Block::Quote(s) => spans_plain(s),
            Block::ListItem(s) => format!("• {}", spans_plain(s)),
            Block::Pre(t) => t.clone(),
            Block::Image { .. } => String::new(),
        }
    }

    /// Plain copy text of a block slice in layout-text coordinates.
    /// The bullet prefix of list items never reaches the clipboard.
    fn copy_slice_of(b: &Block, start: usize, end: usize) -> String {
        let (s, e) = (start.min(end), start.max(end));
        match b {
            Block::Heading(_, spans) | Block::Paragraph(spans) | Block::Quote(spans) => {
                slice_chars(&spans_plain(spans), s, e)
            }
            Block::Pre(t) => slice_chars(t, s, e),
            Block::ListItem(spans) => {
                // Layout text is "• {t}": strip the 2-char prefix.
                let s = s.saturating_sub(2);
                let e = e.saturating_sub(2);
                slice_chars(&spans_plain(spans), s, e)
            }
            Block::Image { .. } => String::new(),
        }
    }

    /// Selected text for the current page in reading order. Partial edge
    /// blocks contribute their selected slice; fully covered blocks
    /// contribute their trimmed text; images are skipped.
    fn selection_text(&self) -> String {
        let Some(((sb, sc), (eb, ec))) = self.ordered_selection() else {
            return String::new();
        };
        let mut parts = Vec::new();
        for idx in sb..=eb {
            let Some(b) = self.flow.get(idx) else {
                continue;
            };
            if matches!(b, Block::Image { .. }) {
                continue;
            }
            let len = Self::layout_text_of(b).chars().count();
            let (s, e) = if sb == eb {
                (sc.min(ec), sc.max(ec))
            } else if idx == sb {
                (sc, len)
            } else if idx == eb {
                (0, ec)
            } else {
                (0, len)
            };
            let piece = Self::copy_slice_of(b, s, e);
            if !piece.trim().is_empty() {
                parts.push(piece.trim().to_string());
            }
        }
        parts.join("\n\n")
    }

    /// Multi-section layout job preserving bold/italic/strike/underline/mono.
    /// `text_color` is WHITE for measurement/hit-testing (size only) and the
    /// theme text color for rendering. Returns `None` for images.
    fn build_job(
        &self,
        b: &Block,
        col_w: f32,
        text_color: egui::Color32,
        code_bg: egui::Color32,
    ) -> Option<egui::text::LayoutJob> {
        use crate::epub_utils::Span;
        use egui::text::{LayoutJob, TextFormat};
        if matches!(b, Block::Image { .. }) {
            return None;
        }
        let w = match b {
            Block::Quote(_) => (col_w - 12.0).max(80.0),
            _ => col_w,
        };
        let size = self.block_size(b);
        let line_height = self.block_line_height(b);
        let base_weight = self.block_weight(b);
        let reader_family = self.reader_family();
        let is_quote = matches!(b, Block::Quote(_));
        let mut job = LayoutJob::default();
        job.wrap.max_width = w;
        job.halign = egui::Align::LEFT;
        job.justify = false;
        let base_format = TextFormat {
            font_id: egui::FontId::new(size, reader_family.clone()),
            line_height: Some(line_height),
            color: text_color,
            coords: egui::epaint::text::VariationCoords::new([(b"wght", base_weight)]),
            ..Default::default()
        };
        let format_for = |span: &Span| -> TextFormat {
            let weight = if span.bold {
                (base_weight + 250.0).min(800.0)
            } else {
                base_weight
            };
            TextFormat {
                font_id: if span.mono {
                    egui::FontId::new(size, egui::FontFamily::Monospace)
                } else {
                    egui::FontId::new(size, reader_family.clone())
                },
                line_height: Some(line_height),
                color: text_color,
                coords: egui::epaint::text::VariationCoords::new([(b"wght", weight)]),
                italics: span.italic || is_quote,
                underline: if span.underline {
                    egui::Stroke::new(1.0, text_color)
                } else {
                    egui::Stroke::NONE
                },
                strikethrough: if span.strike {
                    egui::Stroke::new(1.0, text_color)
                } else {
                    egui::Stroke::NONE
                },
                ..Default::default()
            }
        };
        match b {
            Block::Heading(_, spans) | Block::Paragraph(spans) | Block::Quote(spans) => {
                for s in spans {
                    if s.text.is_empty() {
                        continue;
                    }
                    job.append(&s.text, 0.0, format_for(s));
                }
            }
            Block::ListItem(spans) => {
                job.append("• ", 0.0, base_format.clone());
                for s in spans {
                    if s.text.is_empty() {
                        continue;
                    }
                    job.append(&s.text, 0.0, format_for(s));
                }
            }
            Block::Pre(t) => {
                let mut f = base_format.clone();
                f.font_id = egui::FontId::new(size, egui::FontFamily::Monospace);
                f.background = code_bg;
                job.append(t, 0.0, f);
            }
            Block::Image { .. } => return None,
        }
        if job.sections.is_empty() {
            job.append("", 0.0, base_format);
        }
        Some(job)
    }

    /// Build the layout job for a block exactly like `measure_label`, so
    /// hit-testing and highlight painting agree with what was drawn.
    /// Returns `None` for images (no text layout).
    fn block_selection_job(&self, b: &Block, col_w: f32) -> Option<egui::text::LayoutJob> {
        self.build_job(b, col_w, egui::Color32::WHITE, egui::Color32::TRANSPARENT)
    }

    /// Char offset in a block's layout text for a pointer position.
    /// `rect` is the label rect captured at render time.
    fn block_cursor(
        &self,
        ui: &mut egui::Ui,
        b: &Block,
        rect: egui::Rect,
        col_w: f32,
        pos: egui::Pos2,
    ) -> usize {
        let Some(job) = self.block_selection_job(b, col_w) else {
            return 0;
        };
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        let local = pos - rect.left_top();
        galley.cursor_from_pos(local).index.0
    }

    /// Map a pointer position to a (flow index, char offset) using the
    /// previous frame's block rects. Column-aware: the gutter snaps at its
    /// midpoint so crossing columns immediately tracks the new column by y.
    fn hit_test(&self, ui: &mut egui::Ui, pos: egui::Pos2, col_w: f32) -> Option<(usize, usize)> {
        if self.sel_rects.is_empty() {
            return None;
        }
        let two_col = self.sel_rects.iter().any(|r| r.col == 1);
        let col = if two_col {
            let left_max = self
                .sel_rects
                .iter()
                .filter(|r| r.col == 0)
                .map(|r| r.rect.max.x)
                .fold(f32::NEG_INFINITY, f32::max);
            let right_min = self
                .sel_rects
                .iter()
                .filter(|r| r.col == 1)
                .map(|r| r.rect.min.x)
                .fold(f32::INFINITY, f32::min);
            let mid = if left_max.is_finite() && right_min.is_finite() {
                (left_max + right_min) / 2.0
            } else {
                pos.x
            };
            if pos.x < mid {
                0
            } else {
                1
            }
        } else {
            0
        };
        let mut cands: Vec<&SelBlock> = self.sel_rects.iter().filter(|r| r.col == col).collect();
        if cands.is_empty() {
            return None;
        }
        cands.sort_by(|a, b| {
            a.rect
                .min
                .y
                .partial_cmp(&b.rect.min.y)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // Direct hit first.
        for r in &cands {
            if r.rect.contains(pos) {
                let Some(b) = self.flow.get(r.flow_idx) else {
                    continue;
                };
                let c = self.block_cursor(ui, b, r.rect, col_w, pos);
                return Some((r.flow_idx, c));
            }
        }
        // Above the column: start of its first block.
        if pos.y <= cands[0].rect.min.y {
            return Some((cands[0].flow_idx, 0));
        }
        // Below the column: end of its last block.
        if pos.y >= cands[cands.len() - 1].rect.max.y {
            let last = cands[cands.len() - 1];
            let len = self
                .flow
                .get(last.flow_idx)
                .map(|b| Self::layout_text_of(b).chars().count())
                .unwrap_or(0);
            return Some((last.flow_idx, len));
        }
        // In a gap between blocks: snap to the end of the block above so the
        // selection stays continuous while dragging.
        let mut above = cands[0];
        for r in &cands {
            if r.rect.min.y <= pos.y {
                above = r;
            } else {
                break;
            }
        }
        let len = self
            .flow
            .get(above.flow_idx)
            .map(|b| Self::layout_text_of(b).chars().count())
            .unwrap_or(0);
        Some((above.flow_idx, len))
    }

    /// Whether a pointer position is on the page text (with a small margin
    /// so drags can start at glyph edges).
    fn pointer_in_text(&self, pos: egui::Pos2) -> bool {
        self.sel_rects
            .iter()
            .any(|r| r.rect.expand(4.0).contains(pos))
    }

    /// Paint the current selection as translucent row ranges over the text.
    /// Must run after the page rendered (uses fresh rects + rebuilt galleys).
    fn paint_selection(&self, ui: &mut egui::Ui, rects: &[SelBlock], col_w: f32) {
        use egui::text::CCursor;
        let Some(((sb, sc), (eb, ec))) = self.ordered_selection() else {
            return;
        };
        let mut fill = ui.visuals().selection.bg_fill;
        fill = egui::Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 110);
        for r in rects {
            if r.flow_idx < sb || r.flow_idx > eb {
                continue;
            }
            let Some(b) = self.flow.get(r.flow_idx) else {
                continue;
            };
            let len = Self::layout_text_of(b).chars().count();
            let (s, e) = if sb == eb {
                (sc.min(ec), sc.max(ec))
            } else if r.flow_idx == sb {
                (sc, len)
            } else if r.flow_idx == eb {
                (0, ec)
            } else {
                (0, len)
            };
            let (s, e) = (s.min(len), e.min(len));
            if s >= e {
                continue;
            }
            let Some(job) = self.block_selection_job(b, col_w) else {
                continue;
            };
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            let start = galley.layout_from_cursor(CCursor::new(s));
            let end = galley.layout_from_cursor(CCursor::new(e));
            let (srow, erow) = (start.row.min(end.row), start.row.max(end.row));
            let (scol, ecol) = if start.row <= end.row {
                (start.column, end.column)
            } else {
                (end.column, start.column)
            };
            for ri in srow..=erow {
                let Some(placed) = galley.rows.get(ri) else {
                    continue;
                };
                let left = if ri == srow {
                    placed.row.x_offset(scol)
                } else {
                    0.0
                };
                let right = if ri == erow {
                    placed.row.x_offset(ecol)
                } else {
                    placed.row.size.x
                };
                if right <= left {
                    continue;
                }
                let min = r.rect.left_top() + placed.pos.to_vec2() + egui::vec2(left, 0.0);
                let max =
                    r.rect.left_top() + placed.pos.to_vec2() + egui::vec2(right, placed.row.size.y);
                ui.painter()
                    .rect_filled(egui::Rect::from_min_max(min, max), 0.0, fill);
            }
        }
    }

    /// Pages are built for the open chapter: check the paginated layout
    /// actually matches it (false right after a chapter spill, until the
    /// next frame rebuilds).
    fn pages_built_for_current(&self) -> bool {
        match (&self.reader_book, &self.pages_key) {
            (Some(b), Some(k)) => k.book == b.id && k.chapter == self.current,
            _ => false,
        }
    }

    /// Speak the current page aloud (text only; images are skipped).
    fn speak_current_page(&mut self) {
        let Some(pg) = self.pages.get(self.page).cloned() else {
            return;
        };
        let end = pg.end.min(self.flow.len());
        let start = pg.start.min(end);
        self.speech
            .speak(blocks_to_speech("", &self.flow[start..end]));
    }

    /// Start reading the current page aloud, deferring until pages exist.
    fn start_reading(&mut self) {
        if let Some(e) = self.speech.take_error() {
            self.error = Some(e);
            return;
        }
        self.reading_aloud = true;
        if self.pages_built_for_current() {
            self.speak_current_page();
        } else {
            self.speak_pending = true;
        }
    }

    /// Stop read-aloud and invalidate any in-flight finish events.
    fn stop_reading(&mut self) {
        self.reading_aloud = false;
        self.speak_pending = false;
        self.speech.stop();
    }

    /// User-initiated navigation stops read-aloud first (auto-advance calls
    /// the plain variants directly so the book can read itself).
    fn user_next_page(&mut self) {
        self.stop_reading();
        self.next_page();
    }

    fn user_prev_page(&mut self) {
        self.stop_reading();
        self.prev_page();
    }

    fn user_goto_chapter(&mut self, idx: usize) {
        self.stop_reading();
        self.goto_chapter(idx);
    }

    /// Page turn forward. Spills into the next chapter, like clibrary.
    fn next_page(&mut self) {
        if self.chapters.is_empty() {
            return;
        }
        self.clear_selection();
        let last = self.pages.len().saturating_sub(1);
        if self.page < last || self.pages.is_empty() {
            // saturating_add: `page` can transiently be the usize::MAX
            // end-of-previous-chapter sentinel.
            self.page = self.page.saturating_add(1).min(last);
            self.save_page();
        } else if self.current + 1 < self.chapters.len() {
            self.goto_chapter(self.current + 1);
        }
    }

    /// Page turn backward. Returns to the end of the previous chapter.
    fn prev_page(&mut self) {
        self.clear_selection();
        if self.page > 0 {
            self.page -= 1;
            self.save_page();
        } else if self.current > 0 {
            self.current -= 1;
            // Sentinel: clamped to the real last page once rebuilt next frame.
            self.page = usize::MAX;
            self.pages_key = None;
        }
    }

    fn start_download(&mut self, url: String) {
        if url.trim().is_empty() {
            return;
        }
        if !fanficfare::is_available() {
            self.error =
                Some("fanficfare not found on PATH. Install with `pip install FanFicFare`.".into());
            return;
        }
        self.jobs.busy = true;
        self.jobs.push(format!("downloading {url} …"));
        let tx = self.tx.clone();
        let root = self.lib.root.clone();
        std::thread::spawn(move || {
            let _ = tx.send(JobEvent::Message(format!("downloading {url}")));
            let staging = match tempfile::tempdir() {
                Ok(d) => d,
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(format!("tempdir failed: {e}")));
                    return;
                }
            };
            match fanficfare::download_url(&url, staging.path()) {
                Ok(files) => {
                    let lib = match Library::open(root) {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = tx.send(JobEvent::Error(format!("open library: {e}")));
                            return;
                        }
                    };
                    let mut msgs = vec![];
                    for f in files {
                        match lib.add_file(&f) {
                            Ok(AddOutcome::Added(b)) => msgs.push(format!("added {}", b.title)),
                            Ok(AddOutcome::Duplicate(b)) => {
                                msgs.push(format!("duplicate {}", b.title))
                            }
                            Err(e) => msgs.push(format!("import failed: {e}")),
                        }
                    }
                    let _ = tx.send(JobEvent::Done(msgs.join("; ")));
                    let _ = tx.send(JobEvent::Refresh);
                }
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(e));
                }
            }
        });
    }

    fn start_update_one(&mut self, id: i64) {
        if !fanficfare::is_available() {
            self.error = Some("fanficfare not found on PATH.".into());
            return;
        }
        let Some(book) = self.lib.get_by_id(id).ok().flatten() else {
            return;
        };
        if book.fanfic_url.is_none() {
            self.error = Some("selected book has no FanFicFare URL.".into());
            return;
        }
        self.jobs.busy = true;
        self.jobs.push(format!("updating {} …", book.title));
        let tx = self.tx.clone();
        let root = self.lib.root.clone();
        std::thread::spawn(move || {
            let work = match tempfile::tempdir() {
                Ok(d) => d,
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(format!("tempdir: {e}")));
                    return;
                }
            };
            let tmp_epub = work.path().join("update.epub");
            let lib = match Library::open(root) {
                Ok(l) => l,
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(format!("open lib: {e}")));
                    return;
                }
            };
            let orig = lib.abs_path(&book.file_path);
            if std::fs::copy(&orig, &tmp_epub).is_err() {
                let _ = tx.send(JobEvent::Error("copy for update failed".into()));
                return;
            }
            match fanficfare::update_epub_file(&tmp_epub) {
                Ok(log) => {
                    let _ = tx.send(JobEvent::Message(log.chars().take(500).collect()));
                    match lib.replace_file(book.id, &tmp_epub) {
                        Ok(nb) => {
                            let _ = tx.send(JobEvent::ReaderReload(nb.id));
                            // Freshly imported: local matches remote.
                            let _ = tx.send(JobEvent::CheckDone(vec![(
                                nb.id,
                                fanficfare::CheckState::UpToDate,
                            )]));
                            let remote =
                                crate::epub_utils::chapter_count(&lib.abs_path(&nb.file_path))
                                    .map(|n| n.max(1));
                            let _ = lib.record_check(nb.id, remote);
                            let _ = tx.send(JobEvent::Done(format!("updated {}", nb.title)));
                        }
                        Err(e) => {
                            let _ = tx.send(JobEvent::Error(format!("reimport failed: {e}")));
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(e));
                }
            }
        });
    }

    /// Freshness window for cached update checks: books checked within this
    /// window reuse the cached remote count without network traffic.
    const CHECK_TTL_SECS: i64 = 12 * 3600;

    /// Metadata-only update check over tracked books (session-only results,
    /// backed by a persistent check cache).
    /// AO3 books use a fast native work-page fetch; everything else (and AO3
    /// fallbacks) use FanFicFare. Books with a fresh cached check skip the
    /// network; every attempted fetch is cached for next time.
    fn start_check_updates(&mut self) {
        let tracked = self.lib.tracked().unwrap_or_default();
        if tracked.is_empty() {
            self.error = Some("no tracked fics.".into());
            return;
        }
        self.jobs.busy = true;
        self.jobs.push(format!("checking {} fics …", tracked.len()));
        Self::spawn_update_check(self.tx.clone(), self.lib.root.clone(), false);
    }

    /// Quiet background pass at startup: hydrates markers from cache and
    /// checks stale AO3 books natively. Never touches FanFicFare (slow) and
    /// never takes the busy indicator.
    fn start_silent_update_check(&mut self) {
        if self.tracked_count == 0 {
            return;
        }
        Self::spawn_update_check(self.tx.clone(), self.lib.root.clone(), true);
    }

    /// One attempted remote chapter count: `Some(r)` (cache it; `r` is `None`
    /// when the remote count is definitively unknown) or `None` (nothing
    /// tried or transport failure: do not cache). Silent mode never runs
    /// FanFicFare and stays quiet.
    fn fetch_remote_chapters(
        tx: &mpsc::Sender<JobEvent>,
        title: &str,
        url: &str,
        silent: bool,
        ao3_armed: &mut bool,
    ) -> Option<Option<usize>> {
        if let Some(work_id) = crate::ao3::work_id_from_url(url) {
            if *ao3_armed {
                std::thread::sleep(crate::ao3::POLITENESS_DELAY);
            }
            *ao3_armed = true;
            match crate::ao3::fetch_chapter_count(work_id, crate::ao3::FETCH_TIMEOUT) {
                Ok(count) => return Some(count),
                Err(e) => {
                    if silent {
                        return None;
                    }
                    let _ = tx.send(JobEvent::Message(format!(
                        "check {title}: {e}, trying fanficfare …"
                    )));
                }
            }
        } else if silent {
            // Silent pass stays off the network for non-AO3 books.
            return None;
        }
        if !fanficfare::is_available() {
            if !silent {
                let _ = tx.send(JobEvent::Message(format!(
                    "check {title}: fanficfare not found on PATH"
                )));
            }
            return None;
        }
        match fanficfare::fetch_meta(url, std::time::Duration::from_secs(45)) {
            Ok(meta) => Some(meta.chapters),
            Err(e) => {
                if !silent {
                    let first = e
                        .lines()
                        .next()
                        .unwrap_or(&e)
                        .chars()
                        .take(160)
                        .collect::<String>();
                    let _ = tx.send(JobEvent::Message(format!("check {title} failed: {first}")));
                }
                None
            }
        }
    }

    fn spawn_update_check(tx: mpsc::Sender<JobEvent>, root: std::path::PathBuf, silent: bool) {
        std::thread::spawn(move || {
            let lib = match Library::open(root) {
                Ok(l) => l,
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(format!("open library: {e}")));
                    return;
                }
            };
            let items: Vec<(i64, String, String)> = lib
                .tracked()
                .unwrap_or_default()
                .into_iter()
                .map(|b| (b.id, b.title, b.fanfic_url.unwrap_or_default()))
                .collect();
            if items.is_empty() {
                return;
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let mut results = Vec::new();
            let mut avail = 0;
            let mut cached = 0;
            let mut fetched = 0;
            let mut ao3_armed = false;
            for (id, title, url) in &items {
                if !silent {
                    let _ = tx.send(JobEvent::Message(format!("checking {title} …")));
                }
                let local = lib
                    .get_by_id(*id)
                    .ok()
                    .flatten()
                    .map(|b| {
                        crate::epub_utils::chapter_count(&lib.abs_path(&b.file_path))
                            .unwrap_or(1)
                            .max(1)
                    })
                    .unwrap_or(0);
                // Fresh cache wins: no network.
                let mut remote: Option<Option<usize>> = None;
                if let Ok(Some((at, rem))) = lib.get_check(*id) {
                    if now - at < Self::CHECK_TTL_SECS {
                        remote = Some(rem);
                        cached += 1;
                    }
                }
                let remote = match remote {
                    Some(r) => r,
                    None => {
                        match Self::fetch_remote_chapters(&tx, title, url, silent, &mut ao3_armed) {
                            Some(r) => {
                                // Attempted: cache for next time (ignore cache errors).
                                let _ = lib.record_check(*id, r);
                                fetched += 1;
                                r
                            }
                            // Skipped or failed: never cache a non-attempt.
                            None => continue,
                        }
                    }
                };
                match fanficfare::compare_chapters(local, remote) {
                    Some(state @ fanficfare::CheckState::UpdateAvailable { .. }) => {
                        avail += 1;
                        results.push((*id, state));
                    }
                    Some(state) => results.push((*id, state)),
                    None => {
                        if !silent {
                            let _ = tx.send(JobEvent::Message(format!(
                                "check {title}: no chapter count"
                            )));
                        }
                    }
                }
            }
            let _ = tx.send(JobEvent::CheckDone(results));
            if silent {
                if avail > 0 {
                    let _ = tx.send(JobEvent::Message(format!(
                        "⇪ {avail} fic{} with updates (auto-check)",
                        if avail == 1 { "" } else { "s" }
                    )));
                }
            } else {
                let _ = tx.send(JobEvent::Done(format!(
                    "check complete: {avail} update{} available ({fetched} fetched, {cached} cached)",
                    if avail == 1 { "" } else { "s" }
                )));
            }
        });
    }

    fn start_update_all(&mut self) {
        if !fanficfare::is_available() {
            self.error = Some("fanficfare not found on PATH.".into());
            return;
        }
        let tracked = self.lib.tracked().unwrap_or_default();
        if tracked.is_empty() {
            self.error = Some("no FanFicFare-tracked books.".into());
            return;
        }
        let ids: Vec<(i64, String)> = tracked.into_iter().map(|b| (b.id, b.title)).collect();
        self.start_update_ids(ids, "tracked fics");
    }

    /// Whether every book in the current (possibly filtered) view is checked.
    fn visible_all_checked(&self) -> bool {
        !self.books.is_empty() && self.books.iter().all(|b| self.checked.contains(&b.id))
    }

    /// Check or uncheck every book in the current view. Checked ids outside
    /// the view are left alone.
    fn set_visible_checked(&mut self, on: bool) {
        if on {
            self.checked.extend(self.books.iter().map(|b| b.id));
        } else {
            let ids: Vec<i64> = self.books.iter().map(|b| b.id).collect();
            for id in ids {
                self.checked.remove(&id);
            }
        }
    }

    /// Checked books resolved to (id, title) for updating, plus the count of
    /// checked books that aren't update-eligible.
    fn checked_updatable(&self) -> (Vec<(i64, String)>, usize) {
        let mut ids = Vec::new();
        let mut ineligible = 0;
        for id in &self.checked {
            match self.lib.get_by_id(*id).ok().flatten() {
                Some(b) if is_updatable(&b) => ids.push((b.id, b.title)),
                _ => ineligible += 1,
            }
        }
        (ids, ineligible)
    }

    /// Remove deleted ids from every piece of UI state.
    fn prune_deleted(&mut self, ids: &[i64]) {
        for id in ids {
            self.checked.remove(id);
            self.check_state.remove(id);
            self.textures.remove(&format!("cover-{id}"));
        }
        if self.selected.is_some_and(|s| ids.contains(&s)) {
            self.selected = None;
        }
    }

    /// Update the checked books (select mode). Only FanFicFare-tracked
    /// checked books are eligible; anything else is reported, not run.
    fn start_update_selected(&mut self) {
        if !fanficfare::is_available() {
            self.error = Some("fanficfare not found on PATH.".into());
            return;
        }
        let (ids, ineligible) = self.checked_updatable();
        if ids.is_empty() {
            self.error = Some("no updatable fics selected".into());
            return;
        }
        let note = if ineligible > 0 {
            format!("selected fics ({ineligible} not tracked)")
        } else {
            "selected fics".to_string()
        };
        self.start_update_ids(ids, &note);
    }

    /// Stage a delete confirmation for the checked books (select mode).
    fn ask_delete_checked(&mut self) {
        let mut ids: Vec<(i64, String)> = self
            .books
            .iter()
            .filter(|b| self.checked.contains(&b.id))
            .map(|b| (b.id, b.title.clone()))
            .collect();
        // Checked ids outside the current filter still count.
        for id in &self.checked {
            if !ids.iter().any(|(i, _)| i == id) {
                let title = self
                    .lib
                    .get_by_id(*id)
                    .ok()
                    .flatten()
                    .map(|b| b.title)
                    .unwrap_or_else(|| format!("book {id}"));
                ids.push((*id, title));
            }
        }
        if ids.is_empty() {
            self.delete_confirm = None;
            self.error = Some("no books selected".into());
            return;
        }
        ids.sort_by(|a, b| a.1.cmp(&b.1));
        self.delete_confirm = Some(ids);
    }

    /// Stage a delete confirmation for one book (details pane).
    fn ask_delete_one(&mut self, id: i64) {
        let title = self
            .selected_book()
            .filter(|b| b.id == id)
            .map(|b| b.title.clone())
            .or_else(|| self.lib.get_by_id(id).ok().flatten().map(|b| b.title))
            .unwrap_or_else(|| format!("book {id}"));
        self.delete_confirm = Some(vec![(id, title)]);
    }

    /// Delete the confirmed books: DB rows, files, and all UI state.
    fn do_delete_books(&mut self, ids: &[i64]) {
        if ids.is_empty() {
            self.delete_confirm = None;
            return;
        }
        // Close the reader first if its book is going away.
        if self
            .reader_book
            .as_ref()
            .is_some_and(|b| ids.contains(&b.id))
        {
            self.close_reader();
        }
        match self.lib.delete_books(ids) {
            Ok(n) => {
                self.prune_deleted(ids);
                self.jobs
                    .push(format!("deleted {n} book{}", if n == 1 { "" } else { "s" }));
                self.refresh();
            }
            Err(e) => {
                self.error = Some(format!("delete failed: {e}"));
            }
        }
        self.delete_confirm = None;
    }

    /// Update an explicit id list on a background thread, skipping books a
    /// check already proved up to date. `what` names the set for messages.
    fn start_update_ids(&mut self, ids: Vec<(i64, String)>, what: &str) {
        // Skip books a session check already proved up to date.
        let skip: Vec<i64> = ids
            .iter()
            .filter(|(id, _)| self.check_state.get(id) == Some(&fanficfare::CheckState::UpToDate))
            .map(|(id, _)| *id)
            .collect();
        if skip.len() == ids.len() {
            self.jobs.push(format!("all {what} up to date"));
            return;
        }
        self.jobs.busy = true;
        self.jobs.push(format!(
            "updating {} {what} …{}",
            ids.len() - skip.len(),
            if skip.is_empty() {
                String::new()
            } else {
                format!(" ({} up to date)", skip.len())
            }
        ));
        let tx = self.tx.clone();
        let root = self.lib.root.clone();
        std::thread::spawn(move || {
            let lib = match Library::open(root) {
                Ok(l) => l,
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(format!("open library: {e}")));
                    return;
                }
            };
            let mut ok = 0;
            let mut fail = 0;
            let mut skipped = 0;
            let mut fresh = Vec::new();
            for (id, title) in ids {
                if skip.contains(&id) {
                    skipped += 1;
                    let _ = tx.send(JobEvent::Message(format!("skipped {title} (up to date)")));
                    continue;
                }
                let _ = tx.send(JobEvent::Message(format!("updating {title} …")));
                let Some(book) = lib.get_by_id(id).ok().flatten() else {
                    continue;
                };
                let work = match tempfile::tempdir() {
                    Ok(d) => d,
                    Err(_) => {
                        fail += 1;
                        continue;
                    }
                };
                let tmp_epub = work.path().join("update.epub");
                let orig = lib.abs_path(&book.file_path);
                if std::fs::copy(&orig, &tmp_epub).is_err() {
                    fail += 1;
                    continue;
                }
                match fanficfare::update_epub_file(&tmp_epub) {
                    Ok(_) => match lib.replace_file(id, &tmp_epub) {
                        Ok(nb) => {
                            ok += 1;
                            fresh.push((id, fanficfare::CheckState::UpToDate));
                            // Freshly imported: cache the new local count as remote.
                            let remote =
                                crate::epub_utils::chapter_count(&lib.abs_path(&nb.file_path))
                                    .map(|n| n.max(1));
                            let _ = lib.record_check(id, remote);
                            let _ = tx.send(JobEvent::ReaderReload(id));
                        }
                        Err(_) => fail += 1,
                    },
                    Err(_) => fail += 1,
                }
            }
            if !fresh.is_empty() {
                let _ = tx.send(JobEvent::CheckDone(fresh));
            }
            let _ = tx.send(JobEvent::Done(format!(
                "update: {ok} ok, {fail} failed, {skipped} up to date"
            )));
        });
    }

    fn cover_texture(&mut self, ctx: &egui::Context, book: &Book) -> Option<egui::TextureHandle> {
        let key = format!("cover-{}", book.id);
        if let Some(t) = self.textures.get(&key) {
            return Some(t.clone());
        }
        let (bytes, _) = epub_utils::cover_bytes(&self.lib.root, book)?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let pixels = img.into_flat_samples().samples;
        let color = egui::ColorImage::from_rgba_unmultiplied([w, h], &pixels);
        let tex = ctx.load_texture(key.clone(), color, Default::default());
        self.textures.insert(key, tex.clone());
        Some(tex)
    }
}

impl eframe::App for MarginaliaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_jobs();
        self.set_theme(&ctx);
        if self.view != View::Reader && self.cursor_hidden {
            self.sync_cursor(&ctx, false);
        }

        // Panel edge lines are drawn with the parent painter, so they ignore
        // the fade below; the manual separator at the end fades instead.
        egui::Panel::top("topbar")
            .show_separator_line(false)
            .show(ui, |ui| {
                // In reader mode the whole top bar fades with the chrome; the
                // status line lives in the bottom bar and stays visible.
                if self.view == View::Reader {
                    ui.set_opacity(self.chrome_opacity);
                    if self.chrome_opacity <= 0.05 {
                        ui.disable();
                    }
                }
                ui.horizontal(|ui| {
                    ui.heading("marginalia");
                    ui.separator();
                    if self.view == View::Reader {
                        if ui.button("← Library").clicked() {
                            self.close_reader();
                            self.sync_cursor(ui.ctx(), false);
                        }
                    } else {
                        if ui.button("+ Add").clicked() {
                            self.do_add_files();
                        }
                        if ui.button("⤓ Download").clicked() {
                            self.show_download = true;
                        }
                        if ui
                            .selectable_label(self.select_mode, "☑ Select")
                            .on_hover_text("select mode: check fics for batch update/delete")
                            .clicked()
                        {
                            self.select_mode = !self.select_mode;
                            if !self.select_mode {
                                self.checked.clear();
                                self.delete_confirm = None;
                            }
                        }
                        // Update buttons hide (rather than disable) when the
                        // book can't be update-checked.
                        let fff = fanficfare::is_available();
                        if self.select_mode {
                            let n = self.checked.len();
                            if fff
                                && self.tracked_count > 0
                                && ui
                                    .add_enabled(
                                        n > 0,
                                        egui::Button::new(format!("↻ Update selected ({n})")),
                                    )
                                    .clicked()
                            {
                                self.start_update_selected();
                            }
                            if ui
                                .add_enabled(
                                    n > 0,
                                    egui::Button::new(format!("🗑 Delete selected ({n})")),
                                )
                                .clicked()
                            {
                                self.ask_delete_checked();
                            }
                        } else {
                            if fff
                                && self.selected_book().is_some_and(is_updatable)
                                && ui.button("↻ Update").clicked()
                            {
                                if let Some(b) = self.selected_book() {
                                    let id = b.id;
                                    self.start_update_one(id);
                                } else {
                                    self.error = Some("no book selected".into());
                                }
                            }
                            if fff
                                && self.tracked_count > 0
                                && ui
                                    .button(format!(
                                        "↻ Update all ({} of {})",
                                        self.tracked_count, self.library_total
                                    ))
                                    .clicked()
                            {
                                self.start_update_all();
                            }
                        }
                        if fff && self.tracked_count > 0 && ui.button("⇪ Check").clicked() {
                            self.start_check_updates();
                        }
                        if !fff {
                            ui.weak("fanficfare missing").on_hover_text(
                                "Install with `pip install FanFicFare` for download/update",
                            );
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Display ▾").clicked() {
                            self.show_display = !self.show_display;
                        }
                        if ui.button("?").on_hover_text("help").clicked() {
                            self.show_help = true;
                        }
                        ui.label(format!("{} books", self.books.len()));
                    });
                });
                if self.jobs.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(self.jobs.last());
                    });
                } else if !self.jobs.log.is_empty() {
                    ui.horizontal(|ui| {
                        ui.weak(self.jobs.last());
                    });
                }
                ui.separator();
            });

        if self.show_display {
            let mut open = self.show_display;
            egui::Window::new("Display")
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.set_min_width(260.0);
                    ui.heading("Theme");
                    ui.weak(format!("Current: {}", self.theme.name()));
                    ui.menu_button("Light", |ui| {
                        ui.set_min_width(200.0);
                        for t in ThemeId::all_light() {
                            ui.selectable_value(&mut self.theme, t, t.name());
                        }
                    });
                    ui.menu_button("Dark", |ui| {
                        ui.set_min_width(200.0);
                        for t in ThemeId::all_dark() {
                            ui.selectable_value(&mut self.theme, t, t.name());
                        }
                    });
                    if self.view == View::Reader {
                        ui.separator();
                        ui.heading("Font");
                        for (name, _) in fonts::READER_FONTS {
                            ui.selectable_value(&mut self.reader_font, (*name).to_string(), *name);
                        }
                        ui.separator();
                        ui.add(egui::Slider::new(&mut self.font_size, 12.0..=26.0).text("Size"));
                        ui.add(
                            egui::Slider::new(&mut self.font_weight, 300.0..=700.0)
                                .fixed_decimals(0)
                                .text("Weight"),
                        );
                        ui.separator();
                        ui.heading("Columns");
                        for m in ColumnMode::all() {
                            ui.selectable_value(&mut self.column_mode, *m, m.label());
                        }
                        ui.add(
                            egui::Slider::new(&mut self.max_width, 420.0..=1100.0)
                                .text("Max width"),
                        );
                    }
                });
            self.show_display = open;
        }

        if self.show_download {
            egui::Window::new("Download fic")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label("Story URL (AO3, FFN, RoyalRoad, …)");
                    ui.text_edit_singleline(&mut self.download_url);
                    ui.horizontal(|ui| {
                        if ui.button("Download").clicked() {
                            let url = self.download_url.trim().to_string();
                            self.download_url.clear();
                            self.show_download = false;
                            self.start_download(url);
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_download = false;
                        }
                    });
                });
        }

        if self.delete_confirm.is_some() {
            // Staged ids are cloned out so the dialog can mutate state.
            let pending = self.delete_confirm.clone().unwrap_or_default();
            let mut close = false;
            let mut confirm = false;
            egui::Window::new("Delete fics")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .collapsible(false)
                .show(&ctx, |ui| {
                    ui.label(format!(
                        "Remove {} book{} from the library? Files are deleted.",
                        pending.len(),
                        if pending.len() == 1 { "" } else { "s" }
                    ));
                    for (_, title) in pending.iter().take(5) {
                        ui.weak(format!("• {title}"));
                    }
                    if pending.len() > 5 {
                        ui.weak(format!("…and {} more", pending.len() - 5));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            confirm = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            if confirm {
                let ids: Vec<i64> = pending.iter().map(|(id, _)| *id).collect();
                self.do_delete_books(&ids);
            } else if close {
                self.delete_confirm = None;
            }
        }

        if self.show_help {
            egui::Window::new("Help")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                ui.label("Library: search/sort at the top with the sortable-columns library below, details on the right in wide windows and below in narrow ones. Click a row to select, double-click or Enter to read; Up/Down moves the selection.");
                ui.label("Add: import .epub files into the library.");
                ui.label("Download: fetch a fic URL via fanficfare.");
                ui.label("Update: refresh selected tracked fic (keeps position).");
                ui.label("Update all: refresh every tracked fic (skips books a Check found up to date).");
                ui.label("Check: poll tracked fics' metadata for new chapters (session-only ⇪ markers).");
                ui.label("Select: checkbox mode for batch Update/Delete over checked fics (All selects the filtered view).");
                ui.label("Reader: page turns with Space / ‹ Prev / Next › (or ←/→, PgUp/PgDn), spilling across chapters; Tab toggles the table of contents, R toggles read-aloud; the bottom bar keeps Prev, TOC, and TTS on the left, Next on the right, and the page status centered between them. The top bar and buttons fade out after a few idle seconds, leaving only the status line and bar edges fading with them — move the mouse, scroll, or press a non-page key to bring them back (page turns don't reset the timer). Display settings (18 themes across light/dark sections, font, Auto/One/Two columns — Auto shows two unless too narrow) live in the Display window; click an image for a larger view (click outside, press Esc, or Close to dismiss). Position and display settings auto-save.");
                if ui.button("Close").clicked() {
                    self.show_help = false;
                }
            });
        }

        if let Some(err) = self.error.clone() {
            egui::Window::new("Notice")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(&err);
                    if ui.button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
        }

        match self.view {
            View::Library => self.ui_library(ui),
            View::Reader => self.ui_reader(ui),
        }
        // Read-aloud follow: when the spoken page ends, turn the page and
        // keep reading; stop (rather than loop) at the end of the book. Any
        // backend failure surfaces once as a Notice and stops playback.
        if let Some(e) = self.speech.take_error() {
            self.error = Some(e);
            self.stop_reading();
        }
        if self.reading_aloud && self.view == View::Reader && self.speech.poll_finished() {
            let before = (self.current, self.page);
            self.next_page();
            if (self.current, self.page) == before {
                self.stop_reading();
            } else if self.pages_built_for_current() {
                self.speak_current_page();
            } else {
                // Chapter spill: pages rebuild next frame; speak then.
                self.speak_pending = true;
            }
        }
        self.flush_display_settings();
        if self.jobs.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }
}

impl MarginaliaApp {
    fn ui_library(&mut self, ui: &mut egui::Ui) {
        // Orientation: details go in the right column on wide (landscape)
        // viewports, or the bottom panel on narrow/portrait viewports.
        let landscape = details_on_right(ui.ctx().content_rect().size());

        // One large library section: search + sort controls at the top,
        // sortable-columns table below them.
        // Details of the selected book: right column when landscape,
        // bottom panel when portrait.
        if landscape {
            egui::Panel::right("details")
                .resizable(true)
                .default_size(380.0)
                .show(ui, |ui| {
                    self.show_book_details(ui);
                });
        } else {
            egui::Panel::bottom("details")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| {
                    self.show_book_details(ui);
                });
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Library");
            let search_id = self.search_id;
            let search_changed = ui
                .add(egui::TextEdit::singleline(&mut self.query).id(search_id))
                .changed();
            if search_changed {
                self.refresh();
            }
            ui.horizontal(|ui| {
                ui.label("Sort");
                egui::ComboBox::from_id_salt("sort")
                    .selected_text(self.sort.label())
                    .show_ui(ui, |ui| {
                        for (label, key) in SortKey::all() {
                            ui.selectable_value(&mut self.sort, *key, *label);
                        }
                    });
                if ui
                    .small_button(if self.sort_reverse { "desc" } else { "asc" })
                    .clicked()
                {
                    self.sort_reverse = !self.sort_reverse;
                    self.refresh();
                }
                if ui.small_button("reload").clicked() {
                    self.refresh();
                }
                ui.weak(format!("{} books", self.books.len()));
                if self.select_mode {
                    if ui.small_button("All").clicked() {
                        self.set_visible_checked(true);
                    }
                    if ui.small_button("None").clicked() {
                        self.set_visible_checked(false);
                    }
                    ui.weak(format!(
                        "{} selected{}",
                        self.checked.len(),
                        if self.visible_all_checked() {
                            " (all)"
                        } else {
                            ""
                        }
                    ));
                }
                ui.label("Double-click a row to read.");
            });
            ui.separator();
            if self.books.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("Add EPUBs or download a fic to get started.");
                });
                return;
            }
            self.book_table(ui);
        });

        // Keyboard navigation for the table (Up/Down/Enter), disabled while
        // typing in search or while a dialog is up.
        let search_id = self.search_id;
        let typing = ui.ctx().memory(|m| m.focused() == Some(search_id));
        if !typing && !self.show_download && self.error.is_none() && !self.books.is_empty() {
            let mut step: Option<isize> = None;
            let mut open = false;
            ui.ctx().input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    step = Some(1);
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    step = Some(-1);
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    open = true;
                }
            });
            if let Some(d) = step {
                let n = self.books.len() as isize;
                let idx = self
                    .selected
                    .and_then(|id| self.books.iter().position(|b| b.id == id))
                    .map(|i| i as isize)
                    .unwrap_or(if d > 0 { -1 } else { 0 });
                let next = (idx + d).clamp(0, n - 1) as usize;
                self.selected = self.books.get(next).map(|b| b.id);
            }
            if open {
                if let Some(id) = self.selected {
                    self.open_reader(id);
                }
            }
        }
    }

    /// Sortable details table: Title | Author | Series | Size | Added |
    /// Progress | Fic. Click a row to select, double-click to read.
    fn book_table(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        // Collected during the table build and applied after, since the build
        // borrows the book list.
        let mut sort_request: Option<SortKey> = None;
        let mut open_id: Option<i64> = None;

        let header_label = |key: SortKey, name: &str, active: SortKey, rev: bool| -> String {
            if key == active {
                format!("{name} {}", if rev { "▼" } else { "▲" })
            } else {
                name.to_string()
            }
        };
        let active_sort = self.sort;
        let active_rev = self.sort_reverse;

        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        if self.select_mode {
            builder = builder.column(Column::auto().at_least(28.0));
        }
        builder
            .column(Column::remainder().at_least(140.0))
            .column(Column::auto().at_least(90.0).resizable(true))
            .column(Column::auto().at_least(80.0).resizable(true))
            .column(Column::auto().at_least(64.0))
            .column(Column::auto().at_least(76.0))
            .column(Column::auto().at_least(56.0))
            .column(Column::auto().at_least(32.0))
            .header(26.0, |mut header| {
                if self.select_mode {
                    header.col(|ui| {
                        let all = self.visible_all_checked();
                        if ui
                            .small_button(if all { "☑" } else { "☐" })
                            .on_hover_text("select all / none (filtered view)")
                            .clicked()
                        {
                            self.set_visible_checked(!all);
                        }
                    });
                }
                for (key, name) in [
                    (SortKey::Title, "Title"),
                    (SortKey::Author, "Author"),
                    (SortKey::Series, "Series"),
                    (SortKey::Size, "Size"),
                    (SortKey::Added, "Added"),
                    (SortKey::Progress, "Ch"),
                ] {
                    header.col(|ui| {
                        if ui
                            .button(header_label(key, name, active_sort, active_rev))
                            .clicked()
                        {
                            sort_request = Some(key);
                        }
                    });
                }
                header.col(|ui| {
                    ui.weak("Fic");
                });
            })
            .body(|body| {
                body.rows(20.0, self.books.len(), |mut row| {
                    let idx = row.index();
                    let book = self.books[idx].clone();
                    let missing = !self.lib.abs_path(&book.file_path).exists();
                    let sel = self.selected == Some(book.id);
                    row.set_selected(sel);

                    if self.select_mode {
                        row.col(|ui| {
                            let mut on = self.checked.contains(&book.id);
                            if ui.checkbox(&mut on, "").changed() {
                                if on {
                                    self.checked.insert(book.id);
                                } else {
                                    self.checked.remove(&book.id);
                                }
                            }
                        });
                    }
                    let series = if book.series.is_empty() {
                        String::new()
                    } else if let Some(n) = book.series_index {
                        format!("{} #{n}", book.series)
                    } else {
                        book.series.clone()
                    };
                    let title = if missing {
                        format!("{} (missing)", book.title)
                    } else {
                        book.title.clone()
                    };
                    let progress = if book.progress_chapter > 0 {
                        format!("Ch {}", book.progress_chapter)
                    } else {
                        "—".to_string()
                    };
                    // Session check results surface here: ⇪ means the last
                    // metadata check found new chapters ready.
                    let fic_mark = match self.check_state.get(&book.id) {
                        Some(fanficfare::CheckState::UpdateAvailable { .. }) => "⇪",
                        _ if is_updatable(&book) => "✓",
                        _ => "",
                    };
                    let cells: [String; 7] = [
                        title,
                        book.authors.clone(),
                        series,
                        crate::library::human_size(book.file_size),
                        crate::library::human_date(book.added_at),
                        progress,
                        fic_mark.to_string(),
                    ];
                    for text in cells {
                        row.col(|ui| {
                            let resp = Self::book_cell(ui, sel, missing, &text);
                            if resp.clicked() {
                                self.selected = Some(book.id);
                            }
                            if resp.double_clicked() {
                                open_id = Some(book.id);
                            }
                        });
                    }
                });
            });

        if let Some(key) = sort_request {
            if self.sort == key {
                self.sort_reverse = !self.sort_reverse;
            } else {
                self.sort = key;
                self.sort_reverse = false;
            }
            self.refresh();
        }
        if let Some(id) = open_id {
            self.open_reader(id);
        }
    }

    /// One table cell: full-width click target, dimmed when the file is gone.
    fn book_cell(ui: &mut egui::Ui, selected: bool, missing: bool, text: &str) -> egui::Response {
        ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
            if missing {
                ui.selectable_label(selected, egui::RichText::new(text).weak())
            } else {
                ui.selectable_label(selected, text)
            }
        })
        .inner
    }

    /// Selected-book details pane, shared by the right (landscape) and bottom
    /// (portrait) placements.
    fn show_book_details(&mut self, ui: &mut egui::Ui) {
        let Some(book) = self.selected_book().cloned() else {
            ui.centered_and_justified(|ui| {
                ui.label("Select a book to see details.");
            });
            return;
        };
        ui.horizontal(|ui| {
            let ctx = ui.ctx().clone();
            if let Some(tex) = self.cover_texture(&ctx, &book) {
                ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(120.0, 160.0)));
            }
            ui.vertical(|ui| {
                ui.heading(&book.title);
                ui.label(format!("by {}", book.authors));
                if !book.series.is_empty() {
                    let idx = book
                        .series_index
                        .map(|i| format!(" #{i}"))
                        .unwrap_or_default();
                    ui.label(format!(
                        "{}{idx} · {} · {}",
                        book.series, book.published, book.language
                    ));
                } else if !book.published.is_empty() {
                    ui.label(format!("{} · {}", book.published, book.language));
                }
                ui.weak(format!(
                    "{} · {} · ch.{} p.{}",
                    book.short_id,
                    crate::library::human_size(book.file_size),
                    book.progress_chapter,
                    book.progress_page
                ));
                if let Some(url) = &book.fanfic_url {
                    if !url.trim().is_empty() {
                        ui.hyperlink_to("fic source", url);
                    } else {
                        ui.weak("not a tracked fic");
                    }
                } else {
                    ui.weak("not a tracked fic");
                }
                if let Some(fanficfare::CheckState::UpdateAvailable { new_chapters }) =
                    self.check_state.get(&book.id)
                {
                    ui.label(format!(
                        "⇪ {new_chapters} new chapter{} ready",
                        if *new_chapters == 1 { "" } else { "s" }
                    ));
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.button("Read").clicked() {
                self.open_reader(book.id);
            }
            if fanficfare::is_available() && is_updatable(&book) && ui.button("Update").clicked() {
                self.start_update_one(book.id);
            }
            if ui
                .button("🗑 Delete")
                .on_hover_text("remove this book from the library")
                .clicked()
            {
                self.ask_delete_one(book.id);
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(format!("Publisher: {}", book.publisher));
            ui.label(format!("Subjects: {}", book.subjects));
            ui.separator();
            ui.label(&book.description);
        });
    }

    fn reader_family(&self) -> egui::FontFamily {
        egui::FontFamily::Name(self.reader_font.clone().into())
    }

    /// Font size for a block's label.
    fn block_size(&self, b: &Block) -> f32 {
        match b {
            Block::Heading(1, _) => self.font_size + 8.0,
            Block::Heading(_, _) => self.font_size + 3.0,
            _ => self.font_size,
        }
    }

    /// Line height used when rendering a block.
    fn block_line_height(&self, b: &Block) -> f32 {
        match b {
            Block::Heading(1, _) => (self.font_size + 8.0) * 1.3,
            Block::Heading(_, _) => (self.font_size + 3.0) * 1.3,
            _ => self.font_size * 1.55,
        }
    }

    fn para_gap(&self) -> f32 {
        self.font_size * 0.75
    }

    /// Space above a block. First-in-slice headings sit flush at the top.
    fn lead_gap(&self, b: &Block, is_first: bool) -> f32 {
        if is_first {
            return 0.0;
        }
        match b {
            Block::Heading(1, _) => self.font_size * 1.2,
            Block::Heading(_, _) => self.font_size * 0.9,
            _ => 0.0,
        }
    }

    /// Space below a block. Shared by rendering and pagination so measured
    /// heights match what is drawn.
    fn trail_gap(&self, slice: &[Block], idx: usize) -> f32 {
        match &slice[idx] {
            Block::Heading(1, _) => self.font_size * 0.6,
            Block::Heading(_, _) => self.font_size * 0.45,
            Block::ListItem(_) => {
                let next_is_item = matches!(slice.get(idx + 1), Some(Block::ListItem(_)));
                if next_is_item {
                    2.0
                } else {
                    self.para_gap()
                }
            }
            _ => self.para_gap(),
        }
    }

    /// Rich text for a block, mirroring `render_blocks` styling.
    /// Weight for a block's text. Headings track the body weight a step above
    /// so they stay distinct at any setting (out-of-range values pin to the
    /// font's own limits, e.g. Lora tops out at 700).
    fn block_weight(&self, b: &Block) -> f32 {
        match b {
            Block::Heading(_, _) => (self.font_weight + 150.0).min(800.0),
            _ => self.font_weight,
        }
    }

    /// Layout job for rendering (theme text color), mirroring measurement.
    /// NOTE: headings deliberately do NOT use strong styling via widget
    /// colors; size + weight distinguish them so they stay readable on all
    /// themes.
    fn render_job(&self, ui: &egui::Ui, b: &Block, col_w: f32) -> Option<egui::text::LayoutJob> {
        let visuals = ui.visuals();
        self.build_job(b, col_w, visuals.text_color(), visuals.code_bg_color)
    }

    /// Render a slice of blocks. Returns the clicked image's texture key and
    /// native size, if any, so the caller can open the lightbox.
    /// Text labels are explicitly non-selectable: cross-column drag selection
    /// is handled by the page-level selection state instead of egui's
    /// per-label selection, which can't wrap across the column gutter.
    /// Captured label rects (with `flow_start` as the flow index of
    /// `blocks[0]` and `col` as the column) feed hit-testing and highlight
    /// painting.
    fn render_blocks(
        &self,
        ui: &mut egui::Ui,
        flow_start: usize,
        blocks: &[Block],
        out_rects: &mut Vec<SelBlock>,
        col: u8,
    ) -> Option<(String, u32, u32)> {
        // No inter-widget spacing: gaps come only from explicit add_space calls
        // so rendered heights match pagination measurements exactly.
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut opened = None;
        for (i, b) in blocks.iter().enumerate() {
            let flow_idx = flow_start + i;
            ui.add_space(self.lead_gap(b, i == 0));
            match b {
                Block::Quote(_) => {
                    let mut label_rect = None;
                    // Column width for wrap: captured outside the horizontal so
                    // measurement (col-12) and rendering agree.
                    let col_w = ui.available_width().max(80.0);
                    ui.horizontal(|ui| {
                        ui.separator();
                        // Explicit wrap: labels inside a horizontal layout
                        // default to unwrapped single lines, which would blow
                        // out the column width.
                        if let Some(job) = self.render_job(ui, b, col_w) {
                            let resp = ui.add(egui::Label::new(job).wrap().selectable(false));
                            label_rect = Some(resp.rect);
                        }
                    });
                    if let Some(rect) = label_rect {
                        out_rects.push(SelBlock {
                            flow_idx,
                            rect,
                            col,
                        });
                    }
                    ui.add_space(self.trail_gap(blocks, i));
                }
                Block::Pre(_) => {
                    let col_w = ui.available_width().max(80.0);
                    if let Some(job) = self.render_job(ui, b, col_w) {
                        let resp = ui.add(egui::Label::new(job).wrap().selectable(false));
                        out_rects.push(SelBlock {
                            flow_idx,
                            rect: resp.rect,
                            col,
                        });
                    }
                    ui.add_space(self.trail_gap(blocks, i));
                }
                Block::Image { hash, w, h, .. } => {
                    // Textures are decoded in the ensure pass before drawing;
                    // a missing entry means decode failed, so draw nothing.
                    if let Some(tex) = self.textures.get(&Self::image_key(*hash)) {
                        let col_w = ui.available_width().max(80.0);
                        let (dw, dh) = Self::image_display_size(*w, *h, col_w);
                        let resp = ui.vertical_centered(|ui| {
                            ui.add(
                                egui::Image::new(tex)
                                    .fit_to_exact_size(egui::vec2(dw, dh))
                                    .sense(egui::Sense::click()),
                            )
                        });
                        if resp.inner.clicked() {
                            opened = Some((Self::image_key(*hash), *w, *h));
                        }
                        ui.add_space(self.trail_gap(blocks, i));
                    }
                }
                _ => {
                    let col_w = ui.available_width().max(80.0);
                    if let Some(job) = self.render_job(ui, b, col_w) {
                        let resp = ui.add(egui::Label::new(job).wrap().selectable(false));
                        out_rects.push(SelBlock {
                            flow_idx,
                            rect: resp.rect,
                            col,
                        });
                    }
                    ui.add_space(self.trail_gap(blocks, i));
                }
            }
        }
        opened
    }

    /// Display size of an image block: natural size capped to the column
    /// width (never upscaled, so small ornaments stay crisp), with height
    /// additionally capped so very tall images can't swallow a whole page.
    /// Used identically by measurement and rendering so pages never overflow.
    fn image_display_size(native_w: u32, native_h: u32, col_w: f32) -> (f32, f32) {
        const MAX_H_RATIO: f32 = 1.5;
        if native_w == 0 || native_h == 0 || col_w <= 0.0 {
            return (col_w.clamp(80.0, 200.0), 100.0);
        }
        let mut scale = (col_w / native_w as f32).min(1.0);
        let max_h = col_w * MAX_H_RATIO;
        if native_h as f32 * scale > max_h {
            scale = max_h / native_h as f32;
        }
        (native_w as f32 * scale, native_h as f32 * scale)
    }

    /// Cache key for an image's GPU texture (content hash: identical images
    /// across books and chapters share one upload).
    fn image_key(hash: u64) -> String {
        format!("img-{hash:016x}")
    }

    /// Decode an image block into the GPU texture cache (no-op if cached or
    /// undecodable; decode was already validated at parse time).
    fn cache_image(
        textures: &mut HashMap<String, egui::TextureHandle>,
        ctx: &egui::Context,
        b: &Block,
    ) {
        let Block::Image { bytes, hash, .. } = b else {
            return;
        };
        let key = Self::image_key(*hash);
        if textures.contains_key(&key) {
            return;
        }
        let Ok(dyn_img) = image::load_from_memory(bytes) else {
            return;
        };
        // Cap absurd dimensions before uploading to the GPU.
        let dyn_img = if dyn_img.width() > 2048 || dyn_img.height() > 2048 {
            dyn_img.resize(2048, 2048, image::imageops::FilterType::Triangle)
        } else {
            dyn_img
        };
        let rgba = dyn_img.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let raw = rgba.into_raw();
        let tex = ctx.load_texture(
            key.clone(),
            egui::ColorImage::from_rgba_unmultiplied([w, h], &raw),
            egui::TextureOptions::LINEAR,
        );
        textures.insert(key, tex);
    }

    /// Height of one block's label at `width`, without surrounding gaps.
    fn measure_label(&self, ui: &mut egui::Ui, b: &Block, width: f32) -> (f32, usize) {
        // Images size by aspect ratio, not by text layout.
        if let Block::Image { w, h, .. } = b {
            return (Self::image_display_size(*w, *h, width).1, 1);
        }
        let Some(job) = self.build_job(b, width, egui::Color32::WHITE, egui::Color32::TRANSPARENT)
        else {
            return (0.0, 1);
        };
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        (galley.size().y, galley.rows.len().max(1))
    }

    /// Rebuild a block of the same kind with replacement spans (for fragments
    /// when a paragraph splits across columns/pages).
    fn with_spans(kind: &Block, spans: Vec<crate::epub_utils::Span>) -> Block {
        match kind {
            Block::Paragraph(_) => Block::Paragraph(spans),
            Block::Quote(_) => Block::Quote(spans),
            Block::ListItem(_) => Block::ListItem(spans),
            Block::Heading(n, _) => Block::Heading(*n, spans),
            Block::Pre(_) => Block::Pre(spans_plain(&spans)),
            // Images are never split; this arm is unreachable (see
            // `split_for_height`) and only exists for exhaustiveness.
            Block::Image { .. } => Block::Image {
                bytes: vec![],
                alt: String::new(),
                hash: 0,
                w: 0,
                h: 0,
            },
        }
    }

    /// Split `b` into a leading fragment that fits `max_label_h` plus the
    /// remainder. Returns None for headings and images (never split) and
    /// empty text, or when not even one row fits.
    fn split_for_height(
        &self,
        ui: &mut egui::Ui,
        b: &Block,
        col_w: f32,
        max_label_h: f32,
    ) -> Option<(Block, Block)> {
        // Images (and headings) are never split across pages.
        if matches!(b, Block::Heading(_, _) | Block::Image { .. }) {
            return None;
        }
        // Pre blocks split on plain text; rich blocks split span-aware.
        if let Block::Pre(t) = b {
            if t.trim().is_empty() {
                return None;
            }
            let (full_h, full_rows) = self.measure_label(ui, b, col_w);
            let row_h = full_h / full_rows.max(1) as f32;
            if max_label_h < row_h {
                return None;
            }
            let chars_per_row = (t.chars().count() / full_rows.max(1)).max(1);
            let mut est = (((max_label_h / row_h).floor() as usize).max(1) * chars_per_row).max(16);
            for _ in 0..6 {
                let chunks = epub_utils::chunk_text(t, est);
                if chunks.len() < 2 {
                    est = (est / 2).max(16);
                    continue;
                }
                let head = Block::Pre(chunks[0].clone());
                let tail = Block::Pre(chunks[1..].join(" "));
                let (hh, _) = self.measure_label(ui, &head, col_w);
                if hh <= max_label_h {
                    return Some((head, tail));
                }
                est = ((est as f32 * max_label_h / hh) as usize).max(16);
            }
            return None;
        }
        let spans = match b {
            Block::Paragraph(s) | Block::Quote(s) | Block::ListItem(s) => s,
            Block::Heading(_, _) | Block::Image { .. } | Block::Pre(_) => return None,
        };
        if spans_plain(spans).trim().is_empty() {
            return None;
        }
        let (full_h, full_rows) = self.measure_label(ui, b, col_w);
        let row_h = full_h / full_rows.max(1) as f32;
        if max_label_h < row_h {
            return None;
        }
        let total_chars: usize = spans.iter().map(|s| s.text.chars().count()).sum();
        let chars_per_row = (total_chars / full_rows.max(1)).max(1);
        let mut est = (((max_label_h / row_h).floor() as usize).max(1) * chars_per_row).max(16);
        for _ in 0..6 {
            let chunks = epub_utils::chunk_spans(spans, est);
            if chunks.len() < 2 {
                est = (est / 2).max(16);
                continue;
            }
            let head = Self::with_spans(b, chunks[0].clone());
            // Remaining chunks rejoin with a space so words stay intact.
            let mut tail_spans: Vec<crate::epub_utils::Span> = Vec::new();
            for (ci, ch) in chunks[1..].iter().enumerate() {
                if ci > 0 {
                    // Separator inherits the preceding style when possible.
                    let style = tail_spans
                        .last()
                        .map(|s: &crate::epub_utils::Span| crate::epub_utils::Span {
                            text: String::new(),
                            bold: s.bold,
                            italic: s.italic,
                            strike: s.strike,
                            underline: s.underline,
                            mono: s.mono,
                        })
                        .unwrap_or_default();
                    tail_spans.push(crate::epub_utils::Span {
                        text: " ".to_string(),
                        bold: style.bold,
                        italic: style.italic,
                        strike: style.strike,
                        underline: style.underline,
                        mono: style.mono,
                    });
                }
                tail_spans.extend(ch.clone());
            }
            let tail = Self::with_spans(b, tail_spans);
            let (hh, _) = self.measure_label(ui, &head, col_w);
            if hh <= max_label_h {
                return Some((head, tail));
            }
            est = ((est as f32 * max_label_h / hh) as usize).max(16);
        }
        None
    }

    /// Build `(flow, pages)` for a chapter at the given geometry.
    fn paginate(
        &self,
        ui: &mut egui::Ui,
        heading: &str,
        blocks: &[Block],
        col_w: f32,
        page_h: f32,
        two_col: bool,
    ) -> (Vec<Block>, Vec<PageDef>) {
        let mut flow = vec![Block::Heading(1, plain_spans(heading))];
        flow.extend(blocks.iter().cloned());
        // If the chapter opens with the same heading (common: the TOC label
        // duplicates the in-body h1/h2), drop the duplicate so the title
        // doesn't render twice.
        if let Some(Block::Heading(_, spans)) = flow.get(1) {
            if spans_plain(spans).trim() == heading.trim() {
                flow.remove(1);
            }
        }
        // Label heights/row counts parallel to `flow`, refreshed when a split
        // inserts new fragments.
        let mut mets: Vec<(f32, usize)> = flow
            .iter()
            .map(|b| self.measure_label(ui, b, col_w))
            .collect();
        let cap = page_h * 0.97;
        let ncols = if two_col { 2 } else { 1 };
        let mut pages = Vec::new();
        let mut i = 0;
        while i < flow.len() {
            let start = i;
            let mut split = None;
            for col in 0..ncols {
                if i >= flow.len() {
                    break;
                }
                if col == 1 {
                    split = Some(i);
                }
                let mut rem = cap;
                let mut first = true;
                loop {
                    if i >= flow.len() {
                        break;
                    }
                    let lead = if first {
                        0.0
                    } else {
                        self.lead_gap(&flow[i], false)
                    };
                    let cost = lead + mets[i].0 + self.trail_gap(&flow, i);
                    if cost <= rem + 0.5 {
                        rem -= cost;
                        i += 1;
                        first = false;
                        continue;
                    }
                    // Fresh column and still doesn't fit: place it anyway so a
                    // single oversized block can't stall pagination.
                    let fresh = first && rem >= cap - 0.5;
                    if fresh {
                        i += 1;
                        break;
                    }
                    // Don't strand a heading alone at the bottom of a column.
                    if matches!(flow[i], Block::Heading(_, _)) {
                        break;
                    }
                    // Fill the rest of the column with the block's leading lines.
                    let max_label = rem - lead - self.para_gap();
                    if let Some((head, tail)) =
                        self.split_for_height(ui, &flow[i], col_w, max_label)
                    {
                        let head_m = self.measure_label(ui, &head, col_w);
                        let tail_m = self.measure_label(ui, &tail, col_w);
                        flow[i] = head;
                        flow.insert(i + 1, tail);
                        mets[i] = head_m;
                        mets.insert(i + 1, tail_m);
                        // Head's trail now sees the tail fragment.
                        rem -= lead + head_m.0 + self.trail_gap(&flow, i);
                        i += 1;
                        first = false;
                        continue;
                    }
                    break;
                }
            }
            // A split pointing past the end means the second column is empty.
            if split.is_some_and(|s| s >= i) {
                split = None;
            }
            pages.push(PageDef {
                start,
                end: i,
                split,
            });
        }
        (flow, pages)
    }

    /// Whether two page keys differ only in geometry (column width /
    /// page height). Only geometry changes may wait out a live resize;
    /// anything else rebuilds pages immediately.
    fn same_except_geometry(a: &PagesKey, b: &PagesKey) -> bool {
        a.book == b.book
            && a.chapter == b.chapter
            && a.size_bits == b.size_bits
            && a.weight_bits == b.weight_bits
            && a.family == b.family
            && a.mode == b.mode
    }

    fn use_two_columns(&self, content_w: f32) -> bool {
        match self.column_mode {
            ColumnMode::One => false,
            ColumnMode::Two => true,
            // Auto always shows two columns unless the viewport is too narrow
            // for two minimum-width columns plus the gap.
            ColumnMode::Auto => content_w >= 2.0 * MIN_COLUMN_WIDTH + COLUMN_GAP,
        }
    }

    fn ui_reader(&mut self, ui: &mut egui::Ui) {
        let Some(book) = self.reader_book.clone() else {
            self.sync_cursor(ui.ctx(), false);
            self.view = View::Library;
            return;
        };

        // Auto-fading chrome: mouse movement, scroll, and non-page-turn keys
        // restore full opacity; after a few idle seconds the bars fade out,
        // leaving only the status line. Page turns (buttons, arrows, PgUp/PgDn)
        // deliberately don't reset the timer, so steady reading fades the
        // chrome; clicks only wake already-faded chrome. Checked before key
        // consumption so page turns still work.
        const FADE_DELAY: f32 = 3.0;
        let now = std::time::Instant::now();
        let faded = self.chrome_opacity < 1.0;
        let active = ui.ctx().input(|i| {
            i.pointer.is_moving()
                || (i.pointer.any_pressed() && faded)
                || i.smooth_scroll_delta != egui::Vec2::ZERO
                || i.keys_down.iter().any(|k| !is_page_turn_key(*k))
        });
        if active {
            self.last_activity = now;
        }
        let idle = (now - self.last_activity).as_secs_f32();
        let target = if idle < FADE_DELAY { 1.0 } else { 0.0 };
        // Cursor follows the same target as the chrome: hidden as soon as
        // the fade starts, shown whenever the hide is suppressed.
        self.sync_cursor(ui.ctx(), target == 0.0);
        if self.chrome_opacity != target {
            let dt = ui.ctx().input(|i| i.unstable_dt).clamp(0.0, 0.1);
            let step = dt * 2.5;
            self.chrome_opacity = if self.chrome_opacity < target {
                (self.chrome_opacity + step).min(target)
            } else {
                (self.chrome_opacity - step).max(target)
            };
            ui.ctx().request_repaint();
        } else if target > 0.0 {
            // Still visible: wake up in time to start fading.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f32(
                    (FADE_DELAY - idle).max(0.05),
                ));
        }

        // Keyboard shortcuts, handled before any widget so focused sliders or
        // buttons never also consume these keys. Skipped while typing in a
        // text field (e.g. the Download dialog's URL box).
        //   Space / → / PgDn: next page · ← / PgUp: previous page · Tab: TOC.
        //   Esc: close the image lightbox (and swallow nav keys while it is
        //   open, so pages don't turn behind the overlay).
        let mut close_lightbox = false;
        let mut clear_sel = false;
        let lightbox_open = self.lightbox.is_some();
        if !ui.ctx().text_edit_focused() {
            ui.ctx().input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    if lightbox_open {
                        close_lightbox = true;
                    } else {
                        clear_sel = true;
                    }
                    return;
                }
                if lightbox_open {
                    return;
                }
                let mut turn: Option<i8> = None;
                for (key, dir) in [
                    (egui::Key::Space, 1i8),
                    (egui::Key::ArrowRight, 1),
                    (egui::Key::PageDown, 1),
                    (egui::Key::ArrowLeft, -1),
                    (egui::Key::PageUp, -1),
                ] {
                    if i.consume_key(egui::Modifiers::NONE, key) {
                        turn = Some(dir);
                    }
                }
                match turn {
                    Some(1) => self.user_next_page(),
                    Some(-1) => self.user_prev_page(),
                    _ => {}
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
                    self.toc_open = !self.toc_open;
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::R) {
                    if self.reading_aloud {
                        self.stop_reading();
                    } else {
                        self.start_reading();
                    }
                }
            });
        }
        if close_lightbox {
            self.lightbox = None;
            self.lightbox_new = false;
        }
        if clear_sel {
            self.clear_selection();
        }

        let total_pages = self.pages.len().max(1);
        // NB: `page` can transiently hold the end-of-previous-chapter sentinel
        // (usize::MAX) until the CentralPanel rebuild clamps it below; a plain
        // `+ 1` would overflow and panic here first.
        let status = format!(
            "Page {}/{} · Ch {}/{} · {}",
            self.page.saturating_add(1).min(total_pages),
            total_pages,
            self.current + 1,
            self.chapters.len(),
            book.title
        );
        // Panel edge lines ignore Ui opacity (parent painter), so the bar uses
        // a manual separator that fades with the rest of the chrome.
        egui::Panel::bottom("readerbar")
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.set_opacity(self.chrome_opacity);
                ui.separator();
                let faded = self.chrome_opacity <= 0.05;
                ui.horizontal(|ui| {
                    ui.scope(|ui| {
                        if faded {
                            ui.disable();
                        }
                        if ui.button("‹ Prev").clicked() {
                            self.user_prev_page();
                        }
                        if ui
                            .button(if self.toc_open { "TOC v" } else { "TOC >" })
                            .clicked()
                        {
                            self.toc_open = !self.toc_open;
                        }
                        if ui
                            .button(if self.reading_aloud {
                                "TTS ■"
                            } else {
                                "TTS ▶"
                            })
                            .on_hover_text("read aloud (R)")
                            .clicked()
                        {
                            if self.reading_aloud {
                                self.stop_reading();
                            } else {
                                self.start_reading();
                            }
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.scope(|ui| {
                            if faded {
                                ui.disable();
                            }
                            if ui.button("Next ›").clicked() {
                                self.user_next_page();
                            }
                        });
                        // The status line ignores the fade and stays visible.
                        ui.scope(|ui| {
                            ui.set_opacity(1.0);
                            ui.vertical_centered(|ui| {
                                ui.add(egui::Label::new(status).truncate());
                            });
                        });
                    });
                });
            });
        if self.toc_open {
            egui::Panel::left("toc").show(ui, |ui| {
                ui.heading("Contents");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                        for t in self.toc.clone() {
                            let indent = "  ".repeat(t.depth);
                            let label = format!("{indent}{}", t.label);
                            let is_cur = t.chapter == Some(self.current);
                            if ui.selectable_label(is_cur, label).clicked() {
                                if let Some(c) = t.chapter {
                                    self.user_goto_chapter(c);
                                }
                            }
                        }
                    });
                });
            });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let Some(ch) = self.chapters.get(self.current).cloned() else {
                ui.label("No chapters.");
                return;
            };
            let avail = ui.available_width();
            let content_w = avail.min(self.max_width).max(280.0).min(avail);
            let page_h = ui.available_height().max(120.0);
            let two_col = self.use_two_columns(content_w);
            let page_col_w = if two_col {
                (content_w - COLUMN_GAP) / 2.0
            } else {
                content_w
            };
            self.last_content_w = content_w;
            self.last_page_h = page_h;
            let key = PagesKey {
                book: book.id,
                chapter: self.current,
                col_w: page_col_w.round() as u32,
                page_h: page_h.round() as u32,
                size_bits: self.font_size.to_bits(),
                weight_bits: self.font_weight.to_bits(),
                family: self.reader_font.clone(),
                mode: self.column_mode,
            };
            // A resize is in progress while geometry keeps changing: draw
            // nothing and reflow only once it settles, instead of reshaping
            // text every frame. Anything but a pure geometry change (chapter
            // turn, font/column retune) rebuilds and draws at once.
            let geom = (key.col_w, key.page_h);
            if self.last_geom.replace(geom) != Some(geom) {
                self.last_geom_change = now;
            }
            let resizing = !self.pages.is_empty()
                && now.duration_since(self.last_geom_change)
                    < std::time::Duration::from_millis(RESIZE_SETTLE_MS);
            if self.pages_key.as_ref() != Some(&key) {
                let geometry_only = self
                    .pages_key
                    .as_ref()
                    .is_some_and(|k| Self::same_except_geometry(k, &key));
                if geometry_only && resizing {
                    // Wait for the resize to finish; wake up to draw it.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(RESIZE_SETTLE_MS));
                } else {
                    let (flow, pages) =
                        self.paginate(ui, &ch.label, &ch.blocks, page_col_w, page_h, two_col);
                    self.built_two_col = two_col;
                    self.flow = flow;
                    self.pages = pages;
                    self.pages_key = Some(key);
                    // Flow indices shift when splits insert fragments.
                    self.clear_selection();
                }
            }
            // Deferred read-aloud start (chapter spill or pre-build Read).
            if self.speak_pending && self.pages_built_for_current() {
                self.speak_pending = false;
                if self.reading_aloud {
                    self.speak_current_page();
                }
            }
            // (Compare geometry, not `key`: it was moved above. Equal
            // geometry here means the pages just rebuilt for it.)
            let stale = self
                .pages_key
                .as_ref()
                .is_some_and(|k| (k.col_w, k.page_h) != geom);
            if resizing && stale {
                // Still waiting out the resize: blank content, no render.
                return;
            }
            // Render with the geometry the pages were built for, which lags
            // the window while a live resize settles, so measured and drawn
            // widths can never mismatch mid-drag.
            let (render_col_w, render_content_w) = match &self.pages_key {
                Some(k) => {
                    let cw = k.col_w as f32;
                    let content = if self.built_two_col {
                        cw * 2.0 + COLUMN_GAP
                    } else {
                        cw
                    };
                    (cw, content)
                }
                None => (page_col_w, content_w),
            };
            // Clamp after any geometry change, stale save, or the
            // end-of-previous-chapter sentinel (usize::MAX).
            let last = self.pages.len().saturating_sub(1);
            if self.page > last {
                self.page = last;
                self.save_page();
            }
            let Some(pg) = self.pages.get(self.page).cloned() else {
                ui.label("No text.");
                return;
            };
            // Decode any images on this page into GPU textures before drawing.
            // (Borrowed disjointly: shared `flow`, exclusive `textures`.)
            {
                let ctx = ui.ctx().clone();
                let range = pg.start..pg.end;
                for b in &self.flow[range] {
                    Self::cache_image(&mut self.textures, &ctx, b);
                }
            }
            let pad = ((avail - render_content_w) / 2.0).max(0.0);
            // Cross-column drag selection, driven by the previous frame's
            // block rects (fresh rects are captured during render below).
            // Column-aware hit-testing snaps at the gutter midpoint, so a
            // drag from the bottom of the left column continues at whatever
            // height the pointer enters the right column.
            let selecting_enabled = self.lightbox.is_none() && !ui.ctx().text_edit_focused();
            let pointer_pos = ui.ctx().pointer_interact_pos();
            if selecting_enabled {
                let (pressed, down, released) = ui.ctx().input(|i| {
                    (
                        i.pointer.primary_pressed(),
                        i.pointer.primary_down(),
                        i.pointer.primary_released(),
                    )
                });
                if pressed {
                    if let Some(pos) = pointer_pos {
                        if self.pointer_in_text(pos) {
                            let col_w = self.sel_col_w;
                            if let Some(hit) = self.hit_test(ui, pos, col_w) {
                                self.sel_anchor = Some(hit);
                                self.sel_active = Some(hit);
                                self.sel_dragging = true;
                            }
                        } else {
                            // Click off the text dismisses the selection.
                            self.clear_selection();
                        }
                    }
                } else if self.sel_dragging && down {
                    if let Some(pos) = pointer_pos {
                        let col_w = self.sel_col_w;
                        if let Some(hit) = self.hit_test(ui, pos, col_w) {
                            self.sel_active = Some(hit);
                            ui.ctx().request_repaint();
                        }
                    }
                }
                if released && self.sel_dragging {
                    self.sel_dragging = false;
                    if self.sel_anchor == self.sel_active {
                        self.clear_selection();
                    }
                }
            }
            let mut new_rects: Vec<SelBlock> = Vec::new();
            ui.horizontal(|ui| {
                if pad > 1.0 {
                    ui.add_space(pad);
                }
                ui.vertical(|ui| {
                    ui.set_width(render_content_w);
                    if two_col {
                        // Always two columns in two-column mode: when the
                        // text fits in one, the second column stays empty
                        // rather than collapsing the layout.
                        let mid = pg.split.unwrap_or(pg.end).clamp(pg.start, pg.end);
                        let (left, right) = self.flow[pg.start..pg.end].split_at(mid - pg.start);
                        // Plain top-down columns with an explicit wide
                        // gutter: ragged right, never justified.
                        let mut open_img = None;
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(render_col_w);
                                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                    if let Some(o) =
                                        self.render_blocks(ui, pg.start, left, &mut new_rects, 0)
                                    {
                                        open_img = Some(o);
                                    }
                                });
                            });
                            ui.add_space(COLUMN_GAP);
                            ui.vertical(|ui| {
                                ui.set_width(render_col_w);
                                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                    if let Some(o) =
                                        self.render_blocks(ui, mid, right, &mut new_rects, 1)
                                    {
                                        open_img = Some(o);
                                    }
                                });
                            });
                        });
                        if let Some((key, w, h)) = open_img {
                            self.lightbox = Some(Lightbox {
                                tex_key: key,
                                native_w: w,
                                native_h: h,
                            });
                            self.lightbox_new = true;
                        }
                    } else if let Some((key, w, h)) = self.render_blocks(
                        ui,
                        pg.start,
                        &self.flow[pg.start..pg.end],
                        &mut new_rects,
                        0,
                    ) {
                        self.lightbox = Some(Lightbox {
                            tex_key: key,
                            native_w: w,
                            native_h: h,
                        });
                        self.lightbox_new = true;
                    }
                });
            });
            self.sel_rects = new_rects;
            self.sel_col_w = render_col_w;
            // Selection highlight: painted after the text with a translucent
            // fill so glyphs stay legible.
            self.paint_selection(ui, &self.sel_rects.clone(), render_col_w);
            if selecting_enabled {
                if let Some(pos) = pointer_pos {
                    if self.pointer_in_text(pos) {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                    }
                }
                // Cmd/Ctrl+C copies the selection (egui also emits
                // `Event::Copy` on some backends).
                let want_copy = ui.ctx().input_mut(|i| {
                    let key_copy = i.consume_key(egui::Modifiers::COMMAND, egui::Key::C);
                    let event_copy = i
                        .events
                        .iter()
                        .any(|e| matches!(e, egui::Event::Copy | egui::Event::Cut));
                    key_copy || event_copy
                });
                if want_copy {
                    let text = self.selection_text();
                    if !text.is_empty() {
                        ui.ctx().copy_text(text);
                    }
                }
                // Right-click menu with a Copy item, attached to a hover
                // overlay so it never steals clicks from images.
                if self.ordered_selection().is_some() {
                    let union = self
                        .sel_rects
                        .iter()
                        .fold(egui::Rect::NOTHING, |r, b| r.union(b.rect));
                    if union.is_positive() {
                        let bg = ui.interact(
                            union,
                            egui::Id::unique("reader-sel-bg"),
                            egui::Sense::hover(),
                        );
                        bg.context_menu(|ui| {
                            if ui.button("Copy").clicked() {
                                let text = self.selection_text();
                                if !text.is_empty() {
                                    ui.ctx().copy_text(text);
                                }
                                ui.close();
                            }
                        });
                    }
                }
            }
        });
        self.show_lightbox(ui.ctx());
    }

    /// Window chrome reserved around the lightbox image: frame/margins on
    /// the sides, title bar + Close row + spacing on top/bottom.
    const LIGHTBOX_CHROME: egui::Vec2 = egui::Vec2::new(48.0, 140.0);

    /// Fitted lightbox image size for a `tw` x `th` (points) picture in a
    /// `viewport`, preserving aspect ratio, or `None` for degenerate input.
    /// The whole dialog (image + [`Self::LIGHTBOX_CHROME`]) always fits the
    /// viewport; upscaling is capped so tiny ornaments stay sensible.
    fn lightbox_image_size(tw: f32, th: f32, viewport: egui::Vec2) -> Option<egui::Vec2> {
        if !tw.is_finite() || !th.is_finite() || tw <= 0.0 || th <= 0.0 {
            return None;
        }
        let max = (viewport - Self::LIGHTBOX_CHROME).max(egui::Vec2::splat(1.0));
        let scale = (max.x / tw).min(max.y / th).clamp(0.01, 4.0);
        Some(egui::vec2(tw * scale, th * scale))
    }

    /// Enlarged render of a clicked inline image, drawn as a centered modal
    /// overlay. Displayed at the uploaded texture's own aspect ratio (never
    /// stretched), scaled to fit the viewport, Esc / Close button / click
    /// outside the dialog dismisses it.
    fn show_lightbox(&mut self, ctx: &egui::Context) {
        let Some(lb) = self.lightbox.clone() else {
            return;
        };
        let Some(tex) = self.textures.get(&lb.tex_key).cloned() else {
            self.lightbox = None;
            self.lightbox_new = false;
            return;
        };
        // Aspect ratio comes from the texture that is actually drawn, so the
        // enlarged render can never stretch (native dims label the dialog).
        // Texture size is physical pixels while layout is in points, so
        // divide out the pixel ratio (retina shows 2+ pixels per point) or
        // the image overflows the viewport on HiDPI displays.
        let ppp = ctx.pixels_per_point().max(1.0);
        let tsize = tex.size_vec2() / ppp;
        let (tw, th) = if tsize.x > 0.0 && tsize.y > 0.0 {
            (tsize.x, tsize.y)
        } else if lb.native_w > 0 && lb.native_h > 0 {
            (lb.native_w as f32, lb.native_h as f32)
        } else {
            self.lightbox = None;
            self.lightbox_new = false;
            return;
        };
        let Some(size) = Self::lightbox_image_size(tw, th, ctx.content_rect().size()) else {
            self.lightbox = None;
            self.lightbox_new = false;
            return;
        };
        let mut close = false;
        let win_rect = egui::Window::new("Image")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                // NOTE: no ScrollArea around the image. A scrolling container
                // reports its own (window-bounded) size rather than its content
                // size, so an auto-sized window would never grow to fit the
                // image and the picture would stay cropped with scrolling. An
                // exact-size allocation makes the window size itself to the
                // fitted image instead.
                ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    ui.add(egui::Image::new(&tex).fit_to_exact_size(size));
                });
                ui.horizontal(|ui| {
                    ui.weak(format!("{} × {} px", lb.native_w, lb.native_h));
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            })
            .map(|r| r.response.rect);
        // Clicking anywhere outside the dialog dismisses it. Skipped on the
        // frame the lightbox opened so the opening click can't hit this.
        if !self.lightbox_new {
            if let Some(rect) = win_rect {
                let outside = ctx.input(|i| {
                    i.pointer.any_pressed()
                        && i.pointer
                            .interact_pos()
                            .is_some_and(|pos| !rect.contains(pos))
                });
                if outside {
                    close = true;
                }
            }
        }
        self.lightbox_new = false;
        if close {
            self.lightbox = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts;

    fn fixture_app() -> MarginaliaApp {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().join("lib")).unwrap();
        // Library only contributes paths to pagination; the dir may go away.
        let (tx, rx) = std::sync::mpsc::channel();
        let saved_display = crate::library::DisplaySettings::default();
        MarginaliaApp {
            lib,
            books: vec![],
            tracked_count: 0,
            library_total: 0,
            check_state: HashMap::new(),
            query: String::new(),
            search_id: egui::Id::unique("library-search"),
            sort: SortKey::Recent,
            sort_reverse: false,
            selected: None,
            select_mode: false,
            checked: std::collections::HashSet::new(),
            delete_confirm: None,
            view: View::Library,
            reader_book: None,
            toc: vec![],
            chapters: vec![],
            current: 0,
            page: 0,
            pages: vec![],
            flow: vec![],
            pages_key: None,
            last_content_w: 0.0,
            last_page_h: 0.0,
            toc_open: false,
            font_size: saved_display.font_size,
            font_weight: saved_display.font_weight,
            reader_font: saved_display.font_family.clone(),
            column_mode: ColumnMode::Auto,
            max_width: saved_display.max_width,
            saved_display,
            last_activity: std::time::Instant::now(),
            last_geom: None,
            last_geom_change: std::time::Instant::now(),
            built_two_col: false,
            chrome_opacity: 1.0,
            cursor_hidden: false,
            lightbox: None,
            lightbox_new: false,
            sel_anchor: None,
            sel_active: None,
            sel_dragging: false,
            sel_rects: Vec::new(),
            sel_col_w: 0.0,
            textures: HashMap::new(),
            tx,
            rx,
            jobs: JobState {
                busy: false,
                log: VecDeque::new(),
            },
            speech: SpeechEngine::new(),
            reading_aloud: false,
            speak_pending: false,
            show_download: false,
            download_url: String::new(),
            show_display: false,
            show_help: false,
            theme: ThemeId::DefaultDark,
            error: None,
        }
    }

    fn para_words(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .flat_map(|b| match b {
                Block::Paragraph(s) => spans_plain(s)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    fn paginate_case(
        app: &MarginaliaApp,
        blocks: &[Block],
        col_w: f32,
        page_h: f32,
        two_col: bool,
    ) -> (Vec<Block>, Vec<PageDef>) {
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        // run_ui borrows app immutably; stash the owned result via RefCell.
        let out = std::cell::RefCell::new(None);
        let mut full = ctx.run_ui(egui::RawInput::default(), |ui| {
            *out.borrow_mut() = Some(app.paginate(ui, "Chapter", blocks, col_w, page_h, two_col));
        });
        full.textures_delta.clear();
        out.into_inner().unwrap()
    }

    fn long_chapter() -> Vec<Block> {
        vec![
            Block::Paragraph(plain_spans(("First paragraph. ".repeat(120)).trim())),
            Block::Paragraph(plain_spans(("Second paragraph. ".repeat(120)).trim())),
            Block::Paragraph(plain_spans(("Third paragraph. ".repeat(120)).trim())),
        ]
    }

    #[test]
    fn pages_split_long_paragraphs() {
        let app = fixture_app();
        let blocks = long_chapter();
        let (flow, pages) = paginate_case(&app, &blocks, 500.0, 400.0, false);
        assert!(pages.len() > 1, "long chapter must paginate");
        // Coverage: pages tile the flow exactly, in order, none empty.
        let mut cursor = 0;
        for p in &pages {
            assert!(p.end > p.start, "empty page");
            assert_eq!(p.start, cursor);
            assert!(p.split.is_none());
            cursor = p.end;
        }
        assert_eq!(cursor, flow.len());
        // No words lost or duplicated when fragments split paragraphs.
        assert_eq!(para_words(&blocks), para_words(&flow[1..]));
    }

    #[test]
    fn two_column_pages_stay_ordered() {
        let app = fixture_app();
        let blocks = long_chapter();
        let (flow, pages) = paginate_case(&app, &blocks, 350.0, 400.0, true);
        assert!(pages.len() > 1);
        let mut cursor = 0;
        for p in &pages {
            assert!(p.end > p.start);
            assert_eq!(p.start, cursor);
            if let Some(s) = p.split {
                assert!(s > p.start && s < p.end, "split must be interior");
            }
            cursor = p.end;
        }
        assert_eq!(cursor, flow.len());
        assert_eq!(para_words(&blocks), para_words(&flow[1..]));
    }

    #[test]
    fn no_stranded_headings() {
        let app = fixture_app();
        let blocks = vec![
            Block::Paragraph(plain_spans(("Intro. ".repeat(60)).trim())),
            Block::Heading(2, plain_spans("Section")),
            Block::Paragraph(plain_spans(("Body. ".repeat(200)).trim())),
            Block::Heading(2, plain_spans("Another")),
            Block::Paragraph(plain_spans(("Tail. ".repeat(200)).trim())),
        ];
        for two_col in [false, true] {
            let (flow, pages) = paginate_case(&app, &blocks, 500.0, 420.0, two_col);
            assert!(!pages.is_empty());
            let mut cursor = 0;
            for p in &pages {
                assert!(p.end > p.start);
                assert_eq!(p.start, cursor);
                // A heading may only end a page when nothing follows it.
                if matches!(flow[p.end - 1], Block::Heading(_, _)) {
                    assert_eq!(p.end, flow.len(), "stranded heading");
                }
                cursor = p.end;
            }
            assert_eq!(cursor, flow.len());
            assert_eq!(para_words(&blocks), para_words(&flow[1..]));
        }
    }

    #[test]
    fn empty_chapter_is_one_page() {
        let app = fixture_app();
        let (flow, pages) = paginate_case(&app, &[], 500.0, 400.0, false);
        assert_eq!(flow.len(), 1);
        assert_eq!(pages.len(), 1);
        assert_eq!((pages[0].start, pages[0].end), (0, 1));
    }

    /// Render one slice exactly like a reader column and return how far the
    /// cursor advanced, so rendered height can be compared to pagination.
    fn rendered_height(app: &mut MarginaliaApp, blocks: &[Block], col_w: f32) -> f32 {
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let advance = std::cell::Cell::new(0.0f32);
        let mut full = ctx.run_ui(input, |ui| {
            for b in blocks {
                MarginaliaApp::cache_image(&mut app.textures, &ui.ctx().clone(), b);
            }
            ui.vertical(|ui| {
                ui.set_width(col_w);
                let y0 = ui.cursor().top();
                let mut rects = Vec::new();
                app.render_blocks(ui, 0, blocks, &mut rects, 0);
                advance.set(ui.cursor().top() - y0);
            });
        });
        full.textures_delta.clear();
        advance.get()
    }

    #[test]
    fn quote_blocks_wrap_within_the_column() {
        let mut app = fixture_app();
        // Long enough to wrap several times at 400pt with a 16pt font.
        let blocks = vec![Block::Quote(plain_spans("word ".repeat(200).trim()))];
        let col_w = 400.0;
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let measured = std::cell::Cell::new((0.0f32, 0usize));
        let mut full = ctx.run_ui(input, |ui| {
            measured.set(app.measure_label(ui, &blocks[0], col_w));
        });
        full.textures_delta.clear();
        let (label_h, rows) = measured.get();
        assert!(rows > 3, "test quote should wrap, got {rows} rows");
        let row_h = label_h / rows as f32;
        let rendered = rendered_height(&mut app, &blocks, col_w);
        // Rendered height must match the measured height behind pagination
        // (plus the block's trailing gap): no overflow past the column.
        let expected = label_h + app.trail_gap(&blocks, 0);
        assert!(
            (rendered - expected).abs() < 2.0 * row_h,
            "rendered {rendered} vs measured {expected} (rows={rows})"
        );
    }

    #[test]
    fn image_display_size_caps_sensibly() {
        use MarginaliaApp as A;
        // Small images are never upscaled.
        assert_eq!(A::image_display_size(100, 50, 400.0), (100.0, 50.0));
        // Wide images shrink to the column.
        assert_eq!(A::image_display_size(1600, 400, 400.0), (400.0, 100.0));
        // Very tall images are capped at 1.5x column width.
        assert_eq!(A::image_display_size(300, 900, 400.0), (200.0, 600.0));
        // Degenerate inputs get a small placeholder box, never NaN.
        let (w, h) = A::image_display_size(0, 0, 400.0);
        assert!(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0);
        let (w, h) = A::image_display_size(100, 100, -10.0);
        assert!(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0);
    }

    /// Tiny valid PNG bytes for image tests (solid color, no files needed).
    fn tiny_png(w: u32, h: u32) -> Vec<u8> {
        use image::codecs::png::PngEncoder;
        use image::ImageEncoder;
        let pixels = vec![180u8; (w * h * 4) as usize];
        let mut buf = Vec::new();
        PngEncoder::new(&mut buf)
            .write_image(&pixels, w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        buf
    }

    fn image_block_for_test(w: u32, h: u32) -> Block {
        let bytes = tiny_png(w, h);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&bytes, &mut hasher);
        Block::Image {
            alt: String::new(),
            hash: std::hash::Hasher::finish(&hasher),
            w,
            h,
            bytes,
        }
    }

    #[test]
    fn images_render_at_measured_size() {
        let mut app = fixture_app();
        // 300x100 native at a 400pt column: no upscale, so 300x100.
        let blocks = vec![
            Block::Paragraph(plain_spans("Intro.")),
            image_block_for_test(300, 100),
            Block::Paragraph(plain_spans("Outro.")),
        ];
        let col_w = 400.0;
        // Direct check: the image block measures to its display size.
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let measured = std::cell::Cell::new((0.0f32, 0usize));
        let mut full = ctx.run_ui(input, |ui| {
            measured.set(app.measure_label(ui, &blocks[1], col_w));
        });
        full.textures_delta.clear();
        let (img_h, img_rows) = measured.get();
        assert_eq!((img_h, img_rows), (100.0, 1));
        // And rendering the mixed slice advances exactly by the sum of the
        // measured block heights plus inter-block gaps.
        let rendered = rendered_height(&mut app, &blocks, col_w);
        let para_expected = rendered_height(
            &mut fixture_app(),
            &[Block::Paragraph(plain_spans("Intro."))],
            col_w,
        ) + rendered_height(
            &mut fixture_app(),
            &[Block::Paragraph(plain_spans("Outro."))],
            col_w,
        );
        // Paragraph heights are deterministic for the same text/width, so the
        // mixed render must equal image + both paragraphs + gaps.
        let gap = app.para_gap();
        assert!(
            (rendered - (para_expected + img_h + gap)).abs() < 4.0,
            "rendered {rendered} vs image {img_h} + paras {para_expected}"
        );
    }

    #[test]
    fn headings_resolve_to_visible_colors_on_every_theme() {
        use crate::themes::ThemeId;
        let app = fixture_app();
        for t in ThemeId::all() {
            let visuals = t.visuals();
            for level in [1u8, 2] {
                let b = Block::Heading(level, plain_spans("Chapter Title"));
                // Build via build_job with the theme's text color.
                let job = app.build_job(&b, 500.0, visuals.text_color(), visuals.code_bg_color);
                let job = job.expect("heading job");
                assert!(!job.sections.is_empty(), "{}", t.id());
                // A heading painted in the panel background color is invisible.
                // (Regression test: `.strong()` resolved to the active-widget
                // text color, which themes set near the background color.)
                for s in &job.sections {
                    assert_ne!(s.format.color, visuals.panel_fill, "{}", t.id());
                }
            }
        }
    }

    #[test]
    fn identical_in_body_heading_is_not_duplicated() {
        let app = fixture_app();
        // TOC label "Chapter" matches the in-body h2: the title must appear
        // exactly once at the top of the flow, not twice.
        let blocks = vec![
            Block::Heading(2, plain_spans("Chapter")),
            Block::Paragraph(plain_spans("Body text here.")),
        ];
        let (flow, _) = paginate_case(&app, &blocks, 500.0, 400.0, false);
        assert!(matches!(&flow[0], Block::Heading(1, t) if spans_plain(t) == "Chapter"));
        assert!(matches!(&flow[1], Block::Paragraph(_)));
    }

    #[test]
    fn distinct_in_body_heading_is_kept() {
        let app = fixture_app();
        let blocks = vec![
            Block::Heading(2, plain_spans("A Section Inside")),
            Block::Paragraph(plain_spans("Body text here.")),
        ];
        let (flow, _) = paginate_case(&app, &blocks, 500.0, 400.0, false);
        assert!(matches!(&flow[0], Block::Heading(1, t) if spans_plain(t) == "Chapter"));
        assert!(matches!(&flow[1], Block::Heading(2, t) if spans_plain(t) == "A Section Inside"));
    }

    fn reading_app() -> MarginaliaApp {
        use crate::library::Book;
        let mut app = fixture_app();
        app.view = View::Reader;
        app.reader_book = Some(Book {
            id: 1,
            hash: "abc".to_string(),
            short_id: "abc".to_string(),
            title: "Test Book".to_string(),
            authors: "Author".to_string(),
            series: String::new(),
            series_index: None,
            publisher: String::new(),
            published: String::new(),
            language: "en".to_string(),
            subjects: String::new(),
            description: String::new(),
            fanfic_url: None,
            file_path: "books/Author/Test Book/book.epub".to_string(),
            file_size: 1000,
            added_at: 0,
            opened_at: 0,
            progress_chapter: 0,
            progress_page: 0,
        });
        app.chapters = (0..3)
            .map(|i| Chapter {
                index: i,
                label: format!("Chapter {}", i + 1),
                blocks: vec![
                    Block::Paragraph(plain_spans(("Some body text here. ".repeat(120)).trim())),
                    Block::Paragraph(plain_spans(("More body text here. ".repeat(120)).trim())),
                ],
            })
            .collect();
        app.toc = vec![];
        app.current = 1;
        app.page = 0;
        app
    }

    /// Drive one full reader frame headlessly (fonts installed for measurement).
    fn run_reader_frame(app: &mut MarginaliaApp) {
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let mut full = ctx.run_ui(input, |ui| {
            app.ui_reader(ui);
        });
        full.textures_delta.clear();
    }

    #[test]
    fn prev_across_chapter_boundary_does_not_panic() {
        let mut app = reading_app();
        // First page of chapter 1: no prior page *in this chapter*, so turning
        // back must land on the last page of chapter 0.
        app.prev_page();
        // A full frame must not panic: the status line renders before the
        // CentralPanel rebuild clamps the sentinel page.
        run_reader_frame(&mut app);
        assert_eq!(app.current, 0);
        assert!(!app.pages.is_empty());
        assert!(app.page < usize::MAX);
        assert!(app.page < app.pages.len());
    }

    #[test]
    fn rapid_page_turns_across_chapters() {
        let mut app = reading_app();
        run_reader_frame(&mut app);
        for i in 0..60 {
            if i % 2 == 0 {
                app.next_page();
            } else {
                app.prev_page();
            }
            run_reader_frame(&mut app);
            assert!(app.current < app.chapters.len());
            assert!(!app.pages.is_empty());
            assert!(app.page < app.pages.len());
        }
    }

    #[test]
    fn lightbox_fit_preserves_aspect_and_viewport() {
        use MarginaliaApp as A;
        // Nearly-square 1230x1186 (points) in a 1512x982 viewport: the whole
        // dialog (image + chrome) fits.
        let vp = egui::vec2(1512.0, 982.0);
        let s = A::lightbox_image_size(1230.0, 1186.0, vp).unwrap();
        assert!(
            (s.x / s.y - 1230.0 / 1186.0).abs() < 1e-4,
            "aspect kept: {s:?}"
        );
        assert!(
            s.x + A::LIGHTBOX_CHROME.x <= vp.x + 1.0,
            "fits width: {s:?}"
        );
        assert!(
            s.y + A::LIGHTBOX_CHROME.y <= vp.y + 1.0,
            "fits height: {s:?}"
        );
        // Same picture halved (retina points) in a 1000x685 window.
        let s = A::lightbox_image_size(615.0, 593.0, egui::vec2(1000.0, 685.0)).unwrap();
        assert!(s.x + A::LIGHTBOX_CHROME.x <= 1000.0 + 1.0, "{s:?}");
        assert!(s.y + A::LIGHTBOX_CHROME.y <= 685.0 + 1.0, "{s:?}");
        // Tiny ornaments upscale, but never beyond 4x.
        let s = A::lightbox_image_size(50.0, 40.0, egui::vec2(1600.0, 1200.0)).unwrap();
        assert_eq!(s, egui::vec2(200.0, 160.0));
        // Degenerate input closes the dialog instead of showing NaN.
        assert!(A::lightbox_image_size(0.0, 100.0, vp).is_none());
        assert!(A::lightbox_image_size(f32::NAN, 100.0, vp).is_none());
    }

    #[test]
    fn lightbox_overlay_runs_headless() {
        let mut app = fixture_app();
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let mut full = ctx.run_ui(input(), |ui| {
            let tex =
                ui.ctx()
                    .load_texture("img-test", egui::ColorImage::example(), Default::default());
            app.textures.insert("img-test".to_string(), tex);
        });
        full.textures_delta.clear();
        app.lightbox = Some(Lightbox {
            tex_key: "img-test".to_string(),
            native_w: 64,
            native_h: 64,
        });
        app.lightbox_new = false;
        // No pointer input headless: the dialog must stay open, not crash.
        let mut full = ctx.run_ui(input(), |ui| {
            app.show_lightbox(ui.ctx());
        });
        full.textures_delta.clear();
        assert!(app.lightbox.is_some());
    }

    #[test]
    fn auto_two_columns_ignores_text_length() {
        // Auto shows two columns whenever the viewport fits them, even for
        // chapters too short to need the second one.
        let app = fixture_app();
        assert_eq!(app.column_mode, ColumnMode::Auto);
        assert!(app.use_two_columns(900.0));
        assert!(!app.use_two_columns(500.0));
    }

    #[test]
    fn short_chapter_needs_no_split_in_two_columns() {
        // A tiny chapter fits in one column (split None); the renderer still
        // draws both columns, leaving the second empty.
        let app = fixture_app();
        let blocks = vec![Block::Paragraph(plain_spans("Tiny."))];
        let (flow, pages) = paginate_case(&app, &blocks, 350.0, 400.0, true);
        assert_eq!(pages.len(), 1);
        assert!(pages[0].split.is_none());
        assert_eq!((pages[0].start, pages[0].end), (0, flow.len()));
    }

    fn fixture_book(id: i64, title: &str, fanfic_url: Option<&str>) -> Book {
        Book {
            id,
            hash: format!("hash{id}"),
            short_id: format!("s{id}"),
            title: title.to_string(),
            authors: "Author".to_string(),
            series: String::new(),
            series_index: None,
            publisher: String::new(),
            published: String::new(),
            language: "en".to_string(),
            subjects: String::new(),
            description: String::new(),
            fanfic_url: fanfic_url.map(str::to_string),
            file_path: format!("books/Author/{title}/book.epub"),
            file_size: 1000,
            added_at: 0,
            opened_at: 0,
            progress_chapter: 0,
            progress_page: 0,
        }
    }

    #[test]
    fn select_all_covers_filtered_view_only() {
        let mut app = fixture_app();
        app.select_mode = true;
        app.books = vec![
            fixture_book(1, "A", None),
            fixture_book(2, "B", None),
            fixture_book(3, "C", None),
        ];
        assert!(!app.visible_all_checked());
        app.set_visible_checked(true);
        assert!(app.visible_all_checked());
        assert_eq!(app.checked.len(), 3);
        // Narrowing the view leaves off-filter ids alone on clear.
        app.books.retain(|b| b.id != 3);
        app.set_visible_checked(false);
        assert_eq!(app.checked, std::collections::HashSet::from([3]));
        assert!(!app.visible_all_checked());
    }

    #[test]
    fn ask_delete_checked_sorts_and_counts_off_filter() {
        let mut app = fixture_app();
        app.select_mode = true;
        app.books = vec![
            fixture_book(2, "Zulu", None),
            fixture_book(1, "Alpha", None),
        ];
        app.checked = std::collections::HashSet::from([1, 2, 999]);
        app.ask_delete_checked();
        let pending = app.delete_confirm.clone().unwrap();
        assert_eq!(pending.len(), 3);
        // Sorted by title; unknown off-filter id falls back to a label.
        assert_eq!(pending[0], (1, "Alpha".to_string()));
        assert_eq!(pending[1].0, 2);
        assert_eq!(pending[1].1, "Zulu".to_string());
        assert!(pending.iter().any(|(id, t)| *id == 999 && t == "book 999"));
        assert!(pending.iter().any(|(id, t)| *id == 999 && t == "book 999"));
        // Empty selection errors instead of opening the dialog.
        app.checked.clear();
        app.ask_delete_checked();
        assert!(app.delete_confirm.is_none());
        assert!(app.error.is_some());
    }

    #[test]
    fn prune_deleted_clears_state() {
        let mut app = fixture_app();
        app.checked = std::collections::HashSet::from([1, 2]);
        app.check_state.insert(1, fanficfare::CheckState::UpToDate);
        app.selected = Some(1);
        app.prune_deleted(&[1]);
        assert_eq!(app.checked, std::collections::HashSet::from([2]));
        assert!(app.check_state.is_empty());
        assert!(app.selected.is_none());
        // Untouched ids keep their state.
        app.selected = Some(2);
        app.prune_deleted(&[9]);
        assert_eq!(app.selected, Some(2));
    }

    #[test]
    fn geometry_only_key_comparison() {
        use MarginaliaApp as A;
        let base = PagesKey {
            book: 1,
            chapter: 2,
            col_w: 400,
            page_h: 600,
            size_bits: 0,
            weight_bits: 0,
            family: "Bitter".to_string(),
            mode: ColumnMode::Two,
        };
        // Pure resize: only col/page extents differ.
        let resized = PagesKey {
            col_w: 410,
            page_h: 590,
            ..base.clone()
        };
        assert!(A::same_except_geometry(&base, &resized));
        // Chapter turn or retune must rebuild at once.
        for other in [
            PagesKey {
                chapter: 3,
                ..base.clone()
            },
            PagesKey {
                mode: ColumnMode::One,
                ..base.clone()
            },
            PagesKey {
                family: "Lora".to_string(),
                ..base.clone()
            },
        ] {
            assert!(!A::same_except_geometry(&base, &other));
        }
    }

    #[test]
    fn page_turn_keys_dont_wake_chrome() {
        assert!(is_page_turn_key(egui::Key::ArrowLeft));
        assert!(is_page_turn_key(egui::Key::ArrowRight));
        assert!(is_page_turn_key(egui::Key::PageUp));
        assert!(is_page_turn_key(egui::Key::PageDown));
        assert!(is_page_turn_key(egui::Key::Space));
        assert!(!is_page_turn_key(egui::Key::M));
        assert!(!is_page_turn_key(egui::Key::Enter));
        // Tab toggles the TOC panel, so it counts as chrome interaction.
        assert!(!is_page_turn_key(egui::Key::Tab));
    }

    #[test]
    fn slice_chars_handles_multi_byte_and_clamps() {
        assert_eq!(slice_chars("héllo", 1, 3), "él");
        // Reversed and out-of-range endpoints clamp instead of panicking.
        assert_eq!(slice_chars("abc", 2, 0), "ab");
        assert_eq!(slice_chars("abc", 0, 99), "abc");
        assert_eq!(slice_chars("", 0, 5), "");
    }

    #[test]
    fn list_bullet_prefix_never_reaches_clipboard() {
        let b = Block::ListItem(plain_spans("second item"));
        // Layout text carries the bullet; copy strips it.
        assert_eq!(MarginaliaApp::layout_text_of(&b), "• second item");
        assert_eq!(MarginaliaApp::copy_slice_of(&b, 0, 13), "second item");
        // A range covering only the bullet copies nothing.
        assert_eq!(MarginaliaApp::copy_slice_of(&b, 0, 2), "");
    }

    fn selection_fixture() -> MarginaliaApp {
        let mut app = fixture_app();
        app.flow = vec![
            Block::Paragraph(plain_spans("First block here.")),
            Block::Paragraph(plain_spans("Second block here.")),
            Block::Image {
                bytes: vec![],
                alt: "alt".to_string(),
                hash: 1,
                w: 10,
                h: 10,
            },
            Block::ListItem(plain_spans("third item")),
        ];
        app
    }

    #[test]
    fn selection_text_spans_blocks_and_skips_images() {
        let mut app = selection_fixture();
        // Whole first block plus a slice of the second.
        app.sel_anchor = Some((0, 0));
        app.sel_active = Some((1, 6));
        assert_eq!(app.selection_text(), "First block here.\n\nSecond");
        // Reversed endpoints order the same way.
        app.sel_anchor = Some((1, 6));
        app.sel_active = Some((0, 0));
        assert_eq!(app.selection_text(), "First block here.\n\nSecond");
        // Images contribute no text; the list bullet is stripped.
        app.sel_anchor = Some((1, 0));
        app.sel_active = Some((3, 12));
        assert_eq!(app.selection_text(), "Second block here.\n\nthird item");
        // Empty (tap without drag) selects nothing.
        app.sel_anchor = Some((1, 4));
        app.sel_active = Some((1, 4));
        assert_eq!(app.selection_text(), "");
    }

    #[test]
    fn navigation_clears_selection() {
        let mut app = selection_fixture();
        app.sel_anchor = Some((0, 0));
        app.sel_active = Some((1, 3));
        app.sel_dragging = true;
        app.clear_selection();
        assert!(app.ordered_selection().is_none());
        assert!(!app.sel_dragging);
    }

    #[test]
    fn updatable_requires_nonempty_fanfic_url() {
        fn book_with(url: Option<&str>) -> Book {
            Book {
                id: 1,
                hash: "abc".to_string(),
                short_id: "abc".to_string(),
                title: "T".to_string(),
                authors: "A".to_string(),
                series: String::new(),
                series_index: None,
                publisher: String::new(),
                published: String::new(),
                language: "en".to_string(),
                subjects: String::new(),
                description: String::new(),
                fanfic_url: url.map(str::to_string),
                file_path: "books/A/T/book.epub".to_string(),
                file_size: 1,
                added_at: 0,
                opened_at: 0,
                progress_chapter: 0,
                progress_page: 0,
            }
        }
        assert!(is_updatable(&book_with(Some(
            "https://archiveofourown.org/works/1"
        ))));
        assert!(!is_updatable(&book_with(None)));
        assert!(!is_updatable(&book_with(Some(""))));
        assert!(!is_updatable(&book_with(Some("   "))));
    }
}
