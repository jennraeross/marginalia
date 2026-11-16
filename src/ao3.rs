//! Fast AO3 update checks without FanFicFare.
//!
//! FanFicFare's `--meta-only` fetch pays a Python startup plus a full adapter
//! run per book, which makes library-wide checks painfully slow. For AO3
//! (the common case) the work page itself carries the posted-chapter count in
//! `dl.stats dd.chapters` ("12/?"), so one lightweight GET suffices. Anything
//! AO3 can't answer (locked works, parse failures, non-AO3 hosts) falls back
//! to FanFicFare.

use std::time::Duration;

/// Polite delay between successive AO3 fetches (AO3 throttles aggressively).
pub const POLITENESS_DELAY: Duration = Duration::from_secs(3);
/// Per-fetch timeout for the work page.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Retries on HTTP 429 before giving up to the FanFicFare fallback.
const MAX_ATTEMPTS: u32 = 3;

/// Extract an AO3 work id from a story URL. Accepts work and chapter URLs
/// with any query/fragment (e.g. `.../works/12345`,
/// `.../works/12345/chapters/67890`, `?view_full_work=true`).
pub fn work_id_from_url(url: &str) -> Option<u64> {
    let lower = url.trim().to_ascii_lowercase();
    let host_start = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let host_end = host_start.find('/').unwrap_or(host_start.len());
    if host_start[..host_end] != *"archiveofourown.org" {
        return None;
    }
    let path = &host_start[host_end..];
    let marker = "/works/";
    let at = path.find(marker)?;
    let rest = &path[at + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok()
}

/// Posted-chapter count from a work page's stats blurb ("12/?" -> 12).
/// Returns `None` when the page has no parseable count (locked works,
/// consent interstitials, unexpected markup) so the caller can fall back.
pub fn parse_chapter_count(html: &str) -> Option<usize> {
    if html.is_empty() {
        return None;
    }
    let doc = scraper::Html::parse_document(html);
    let sel = scraper::Selector::parse("dl.stats dd.chapters, dd.chapters").ok()?;
    let el = doc.select(&sel).next()?;
    let text: String = el.text().collect();
    let current = text.split('/').next()?.trim();
    if current.is_empty() {
        return None;
    }
    current.parse::<usize>().ok()
}

/// Fetch the posted-chapter count for an AO3 work id.
/// `Ok(None)` means "unknown, use the FanFicFare fallback" (locked work,
/// unexpected markup); `Err` means the fetch itself failed.
pub fn fetch_chapter_count(work_id: u64, timeout: Duration) -> Result<Option<usize>, String> {
    let url = format!("https://archiveofourown.org/works/{work_id}?view_adult=true");
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build();
    let agent: ureq::Agent = config.into();
    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let res = agent
            .get(&url)
            .header(
                "User-Agent",
                "marginalia/0.1 (update check; +https://github.com)",
            )
            .call();
        match res {
            Ok(mut response) => {
                let body = response
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| format!("read work page failed: {e}"))?;
                return Ok(parse_chapter_count(&body));
            }
            Err(ureq::Error::StatusCode(429)) if attempt < MAX_ATTEMPTS => {
                last_err = "rate limited (429)".to_string();
                std::thread::sleep(Duration::from_secs(5 * attempt as u64));
            }
            Err(ureq::Error::StatusCode(code)) => {
                // 404 (deleted) and other statuses carry no count; the
                // FanFicFare path can't do better, so report unknown.
                let _ = code;
                return Ok(None);
            }
            Err(e) => {
                last_err = format!("AO3 fetch failed: {e}");
                if attempt < MAX_ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_id_shapes() {
        assert_eq!(
            work_id_from_url("https://archiveofourown.org/works/12345"),
            Some(12345)
        );
        assert_eq!(
            work_id_from_url("http://archiveofourown.org/works/12345/chapters/67890"),
            Some(12345)
        );
        assert_eq!(
            work_id_from_url("https://archiveofourown.org/works/98765?view_full_work=true"),
            Some(98765)
        );
        assert_eq!(
            work_id_from_url("https://archiveofourown.org/works/11223#main"),
            Some(11223)
        );
        // Non-AO3 hosts are not AO3 works.
        assert_eq!(
            work_id_from_url("https://www.fanfiction.net/s/12345/1/x"),
            None
        );
        assert_eq!(work_id_from_url("https://example.com/works/12345"), None);
        // No numeric id.
        assert_eq!(work_id_from_url("https://archiveofourown.org/works/"), None);
        assert_eq!(work_id_from_url("not a url"), None);
        assert_eq!(work_id_from_url(""), None);
    }

    fn stats_page(chapters: &str) -> String {
        format!(
            r#"<html><body><dl class="stats"><dt>Chapters:</dt><dd class="chapters">{chapters}</dd></dl></body></html>"#
        )
    }

    #[test]
    fn chapter_count_shapes() {
        assert_eq!(parse_chapter_count(&stats_page("12/?")), Some(12));
        assert_eq!(parse_chapter_count(&stats_page("5/5")), Some(5));
        assert_eq!(parse_chapter_count(&stats_page("1/1")), Some(1));
        assert_eq!(parse_chapter_count(&stats_page(" 7 / 20 ")), Some(7));
    }

    #[test]
    fn chapter_count_unknown_stays_none() {
        // Locked-work page has no stats blurb.
        assert_eq!(
            parse_chapter_count("<html><body><p>registered users only</p></body></html>"),
            None
        );
        assert_eq!(parse_chapter_count(""), None);
        assert_eq!(parse_chapter_count(&stats_page("?/?")), None);
    }
}
