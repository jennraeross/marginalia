# marginalia

Cross-platform EPUB library + reader with FanFicFare support, written in Rust + egui.
GUI successor to `clibrary` (add / browse / read / download / update MVP).

## Run (macOS v1)

```
cargo run
```

Library defaults to `~/Library/Application Support/marginalia`.

## Features (MVP)

- **Add**: import `.epub` files via file picker. SHA-256 verified copy, dedupe, metadata (title/authors/series/publisher/date/language/subjects/description), cover extraction. Layout: `books/Author/Title/book.epub`.
- **Browse**: search/sort at the top with the sortable-columns library below (search across title/author/series/subjects/description/id, click headers to sort, double-click or Enter to read, Up/Down to move), details pane on the right when landscape or the bottom when portrait, with cover.
- **Read**: paginated in-app reader, XHTML → headings/paragraphs/quotes/lists/pre/**inline images** (raster formats; remote and SVG images are skipped), TOC (NCX/nav fallback to headings), page turns with Space / ‹ Prev / Next › (or ←/→, PgUp/PgDn; Tab toggles the TOC), spilling across chapters like clibrary, bottom bar with Prev/TOC/Next and a centered status line that stays visible while the rest of the chrome auto-fades after a few idle seconds (page turns don't reset the fade; R or the Read button reads the book aloud page by page via the OS speech engine, auto-advancing and stopping at the end or on manual navigation), Display window (18 themes: Default, Gruvbox, Solarized, Dracula, Nord, Catppuccin ×4, Everforest, Rosé Pine ×3 — in Light/Dark sections — plus font family (Bitter default, plus Literata, Lora, Inter — all OFL) + size + variable weight, Auto/One/Two columns with adjustable max width and a wide inter-column gap; clicking an image opens a larger render at its own aspect ratio (click outside, Esc, or Close to dismiss). Chapter+page position and all display settings auto-save to SQLite (defaults: Bitter 18pt/450wt, Auto columns, 900px max, Default Dark).
- **Download**: story URL → `fanficfare --non-interactive <url>` in temp staging dir → auto-import. Requires `pip install FanFicFare`.
- **Update**: selected or all FanFicFare-tracked books via `fanficfare -u` on a temp copy, then re-import. Reading position preserved.

SQLite storage: `index.db` (`books` table with progress + settings). No CLI/JSON export in v1.

## Dev

```
cargo test
cargo fmt --check
cargo clippy --all-targets
```

Bundled fonts live in `assets/fonts/` (SIL Open Font License, sourced from Google Fonts): Bitter, Literata, Lora, Inter.
