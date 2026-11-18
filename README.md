# marginalia

![CI](https://github.com/jennraeross/marginalia/actions/workflows/ci.yml/badge.svg)
![Release](https://github.com/jennraeross/marginalia/actions/workflows/release.yml/badge.svg)

Cross-platform EPUB library + reader with FanFicFare support, written in Rust + egui.

## Install

Download the installer for your platform from
[GitHub Releases](https://github.com/jennraeross/marginalia/releases):
macOS (Apple Silicon `.dmg`), Windows (`.exe`), Linux (`.deb`).

Or run from source (below).

## Run

```sh
cargo run
```

Library location defaults to `~/Library/Application Support/marginalia` on macOS,
otherwise the platform data dir for `marginalia` (e.g. `~/.local/share/marginalia`
on Linux), falling back to `.marginalia`. The window opens at 1100×750.

Requires `pip install FanFicFare` only for Download / Update; everything else
works without it.

## Library

- **Add**: import `.epub` files via the file picker. Files are SHA-256 hashed,
  deduplicated, and verified after copy, then stored as
  `books/Author/Title/book.epub` with the extracted cover alongside
  (`cover.jpg/png/gif/webp`). Metadata captured: title, authors, series +
  index, publisher, date, language, subjects, description, and FanFicFare URL.
- **Browse**: search across title/author/series/subjects/description/id, with
  sortable columns (Recent, Author, Title, Series, Published, Added, Size,
  Progress, Update). Double-click or Enter reads; Up/Down moves. A details pane
  shows metadata plus cover (right side in landscape, bottom in portrait).
- **Select mode**: checkboxes for batch update-checks and batch delete.
- Storage is SQLite (`index.db`): `books` (metadata + progress), `annotations`
  (highlights/notes), update-check cache, and `settings` (including display).

## Reader

- Paginated reader over XHTML chapters: headings, paragraphs, block quotes,
  lists, `pre`, and **inline raster images** (remote and SVG images are
  skipped). Clicking an image opens a larger render at its own aspect ratio
  (click outside, Esc, or Close to dismiss).
- TOC from NCX, falling back to the EPUB3 nav document and then to headings.
  Page turns spill across chapters.
- Keyboard: Space / `‹ Prev` / `Next ›` (or `←`/`→`, `PgUp`/`PgDn`); `Tab`
  toggles the TOC; `R` or the TTS button reads aloud. Top bar
  (Library/TOC/Notes/TTS) and bottom bar (Prev/Next + centered status) auto-fade
  after a few idle seconds; page turns don't reset the fade.
- Chapter + page position auto-saves per book.

## Notes

- Drag-select text to create a highlight (yellow, green, pink, blue) with an
  optional note. The Notes panel lists a book's annotations, jumps to them in
  the reader, and supports edit/delete.
- Anchors are stored per chapter href with quote + surrounding context, so
  highlights relocate fuzzily across FanFicFare re-imports.

## Display

Display window: font family (Bitter default, plus Literata, Lora, Inter),
size, variable weight, Auto/One/Two columns with adjustable max width, and one
of 18 themes grouped into Light/Dark sections:

Default, Gruvbox, Solarized, Dracula, Nord, Catppuccin (Mocha, Frappé,
Macchiato, Latte), Everforest, Rosé Pine (Pine, Moon, Dawn).

Defaults: Bitter 18pt/450wt, Auto columns, 900px max, Default Dark. All display
settings persist to SQLite.

## Read aloud (TTS)

`R` or the TTS button reads the book aloud page by page via the OS speech
engine, auto-advancing and stopping at the end or on manual navigation.

## Download

Story URL → `fanficfare --non-interactive <url>` in a temp staging dir →
auto-import. Requires `pip install FanFicFare`. AO3 preface links are detected
at import, so direct AO3 downloads become tracked books even without a URL
identifier in the EPUB metadata.

## Update checks

- Tracked books (those with a FanFicFare source URL) can be checked for new
  chapters. AO3 works get a fast native chapter-count fetch; results persist
  in a check cache (`record_check`/`get_check`) so repeat checks are cheap.
- The Update sort tier surfaces `UpdateAvailable` first, then tracked-but-
  unchecked books, then up-to-date, then untracked.
- **Update** runs `fanficfare -u` on a temp copy of selected (or all tracked)
  books, then re-imports. Reading progress is preserved (chapter clamped if the
  book shrank).

## Dev

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

EPUB parsing lives in `src/epub_core.rs` (internal, MIT): spine/chapter HTML,
NCX + EPUB3 nav TOC, Dublin Core / calibre metadata, cover lookup, and
manifest resource reads over `zip` + `roxmltree`.

Bundled fonts live in `assets/fonts/` (SIL Open Font License, sourced from
Google Fonts): Bitter, Literata, Lora, Inter.

## License

MIT — see [LICENSE](LICENSE).
