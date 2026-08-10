//! Fetch Copilot seat assignments from GitHub (RFC-0001's headline use
//! case: "who has a seat and has never used it").
//!
//! Unlike the daily reports (`report.rs`), `/orgs/{org}/copilot/billing/
//! seats` is a plain paginated JSON REST response -- no envelope, no
//! signed download URL, one GET per page, `Link: rel="next"` to continue.
//! GitHub gives this endpoint no `day` parameter at all: it always returns
//! the CURRENT seat assignments, so there is no client-level notion of
//! "day" here -- that only exists once a caller decides to stamp a fetch
//! with today's date (see `sync::sync_seats`).
//!
//! Pagination is bounded on two independent axes so neither a malformed
//! `Link` header nor a server that always claims another page can hang a
//! run: `MAX_SEAT_PAGES` caps the page count, and `MAX_SEATS_PAGE_BYTES`
//! caps each page's body while streaming (mirrors `report.rs`'s
//! `MAX_REPORT_BYTES` guard).

use reqwest::header::LINK;

use crate::{
    client::GithubClient,
    error::{CopilotError, Result},
    secret::RawSecret,
};

/// Seats requested per page (GitHub's documented max for this endpoint).
const SEATS_PER_PAGE: u32 = 100;

/// Hard cap on pages followed for one org's seat listing. At 100
/// seats/page this is 20,000 seats -- far beyond any real org (spike-0007's
/// probe org had 64 filled seats) -- so this exists purely to bound a
/// malformed or looping `Link` header, not to reject a legitimately large
/// org.
const MAX_SEAT_PAGES: usize = 200;

/// Hard cap on one page's response body, in bytes. A page of 100 seats is a
/// few tens of KB; this is a memory guard against a misbehaving host, not a
/// throughput limit (mirrors `report.rs`'s `MAX_REPORT_BYTES`).
const MAX_SEATS_PAGE_BYTES: usize = 16 * 1024 * 1024;

/// The org's Copilot seat listing, in raw page order. Kept as raw bytes
/// (not parsed) so the caller can archive them BEFORE parsing, matching
/// `fetch_report`'s ordering (RFC-0001: replay, not refetch). `Debug` is
/// safe to derive: every field here is bytes/page-count, no token or
/// signed URL (there is no signed URL on this endpoint at all).
#[derive(Debug)]
pub struct FetchedSeats {
    pub pages: Vec<Vec<u8>>,
}

impl FetchedSeats {
    /// Combine every page's raw bytes into one archivable JSON document: a
    /// JSON array of the raw per-page response bodies, in page order. Each
    /// page is already valid JSON on its own, so wrapping with `[`/`,`/`]`
    /// (rather than newline-joining, which would risk corrupting the
    /// archive if a page body ever contained a literal newline byte)
    /// produces a single valid JSON document `parse::parse_seats` can read
    /// back directly -- whether called from the live fetch path or a
    /// future replay of this exact archive.
    pub fn to_archive_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            self.pages.iter().map(Vec::len).sum::<usize>() + self.pages.len() + 2,
        );
        out.push(b'[');
        for (i, page) in self.pages.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            out.extend_from_slice(page);
        }
        out.push(b']');
        out
    }
}

impl GithubClient {
    /// Fetch every page of `/orgs/{org}/copilot/billing/seats`, following
    /// `Link: rel="next"`. Always starts at page 1 -- there is no
    /// high-water mark for a listing that carries no history at all.
    pub async fn fetch_seats(&self, org: &str, token: &RawSecret) -> Result<FetchedSeats> {
        let mut pages = Vec::new();
        let mut next_url = Some(format!(
            "{}/orgs/{org}/copilot/billing/seats?per_page={SEATS_PER_PAGE}&page=1",
            self.api_base()
        ));
        let mut page_no = 0usize;

        while let Some(url) = next_url.take() {
            page_no += 1;
            if page_no > MAX_SEAT_PAGES {
                return Err(CopilotError::github(
                    "copilot/billing/seats",
                    0,
                    format!(
                        "exceeded {MAX_SEAT_PAGES} pages without exhausting pagination; \
                         aborting rather than following a Link header forever"
                    ),
                ));
            }

            let resp = self
                .send_with_retry(|| {
                    self.inner()
                        .get(&url)
                        .bearer_auth(token.as_ref())
                        .header("Accept", "application/vnd.github+json")
                        .header("X-GitHub-Api-Version", crate::API_VERSION)
                })
                .await?;
            let status = resp.status().as_u16();
            // Grab the `Link` header before the body read below consumes
            // `resp` -- same "capture what we need, then drop the
            // response" shape as `report.rs`'s host-from-signed-URL step.
            let link = resp
                .headers()
                .get(LINK)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);

            let bytes = read_capped_body(resp, page_no).await?;
            if status != 200 {
                let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
                let msg = json
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map_or_else(|| "no message".to_owned(), str::to_owned);
                return Err(CopilotError::github("copilot/billing/seats", status, msg));
            }

            pages.push(bytes);
            next_url = link.as_deref().and_then(next_link_url);
        }

        Ok(FetchedSeats { pages })
    }
}

/// Read a response body, enforcing `MAX_SEATS_PAGE_BYTES` while streaming --
/// a lying (or absent) `Content-Length` must not bypass the cap, so the
/// check runs both up front (fail fast on an honest header) and per chunk
/// while accumulating (mirrors `report.rs::fetch_report`'s guard).
async fn read_capped_body(mut resp: reqwest::Response, page_no: usize) -> Result<Vec<u8>> {
    let too_large = |size: u64| {
        CopilotError::github(
            "copilot/billing/seats",
            0,
            format!(
                "page {page_no} body was {size} bytes, over the {MAX_SEATS_PAGE_BYTES} byte cap"
            ),
        )
    };
    if let Some(len) = resp.content_length()
        && len > MAX_SEATS_PAGE_BYTES as u64
    {
        return Err(too_large(len));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(CopilotError::transport)? {
        let next = bytes.len() + chunk.len();
        if next > MAX_SEATS_PAGE_BYTES {
            return Err(too_large(next as u64));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Extract the `rel="next"` URL from a raw `Link` header value (RFC 8288:
/// `<url>; rel="next", <url>; rel="last"`). Returns `None` on anything that
/// does not parse as a `rel="next"` entry -- a malformed header must stop
/// pagination outright, not be treated as "keep going" by accident.
fn next_link_url(header: &str) -> Option<String> {
    header.split(',').find_map(|entry| {
        let mut segments = entry.split(';').map(str::trim);
        let bracketed = segments.next()?;
        let is_next = segments.any(|s| s.eq_ignore_ascii_case("rel=\"next\""));
        if !is_next {
            return None;
        }
        bracketed
            .strip_prefix('<')?
            .strip_suffix('>')
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_link_url_extracts_the_next_relation() {
        let header = concat!(
            "<https://api.github.com/orgs/o/copilot/billing/seats?page=2>; rel=\"next\", ",
            "<https://api.github.com/orgs/o/copilot/billing/seats?page=5>; rel=\"last\""
        );
        assert_eq!(
            next_link_url(header),
            Some("https://api.github.com/orgs/o/copilot/billing/seats?page=2".to_owned())
        );
    }

    #[test]
    fn next_link_url_is_none_without_a_next_relation() {
        let header = "<https://api.github.com/orgs/o/copilot/billing/seats?page=1>; rel=\"last\"";
        assert_eq!(next_link_url(header), None);
    }

    #[test]
    fn next_link_url_is_none_for_a_malformed_header() {
        assert_eq!(next_link_url("not-a-link-header-at-all"), None);
        assert_eq!(next_link_url(""), None);
    }

    #[test]
    fn to_archive_bytes_produces_a_valid_json_array_of_the_raw_pages() {
        let fetched = FetchedSeats {
            pages: vec![
                br#"{"seats":[{"a":1}]}"#.to_vec(),
                br#"{"seats":[{"b":2}]}"#.to_vec(),
            ],
        };
        let archived = fetched.to_archive_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&archived).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }
}
