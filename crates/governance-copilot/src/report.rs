//! Fetch Copilot report NDJSON from GitHub.
//!
//! The report endpoints return an envelope with short-lived signed download
//! URLs; the NDJSON lives behind those. We download within the same run
//! (RFC-0001) and hand the raw bytes back to the caller, which is responsible
//! for archiving them to S3 and parsing. We never log the signed URL -- the
//! only thing derived from it is its host (for egress verification).

use crate::{
    ReportEnvelope,
    client::GithubClient,
    error::{CopilotError, Result},
    secret::RawSecret,
};

/// Response to a report fetch: the raw NDJSON (to archive + parse) plus the
/// signed-download host (used once for egress verification, never the URL).
pub struct DownloadedReport {
    /// The raw NDJSON bytes of the report (empty when GitHub returned 204).
    pub bytes: Vec<u8>,
    /// The host the signed URL pointed at (e.g. `copilot-reports.github.com`).
    pub host: Option<String>,
    /// True when the report endpoint returned 204 (a day with no data yet).
    pub empty: bool,
}

impl GithubClient {
    /// Fetch the report envelope for `report` on `day`, then download the
    /// NDJSON behind its signed URL, then drop the URL.
    pub async fn fetch_report(
        &self,
        org: &str,
        report: &str,
        day: &str,
        token: &RawSecret,
    ) -> Result<DownloadedReport> {
        let url =
            format!("https://api.github.com/orgs/{org}/copilot/metrics/reports/{report}?day={day}");
        let resp = self
            .inner()
            .get(&url)
            .bearer_auth(token.as_ref())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", crate::API_VERSION)
            .send()
            .await
            .map_err(CopilotError::Transport)?;
        let status = resp.status().as_u16();

        // 204 = valid "no data for this day". Return empty, not failure.
        if status == 204 {
            return Ok(DownloadedReport {
                bytes: Vec::new(),
                host: None,
                empty: true,
            });
        }
        let text = resp.text().await.map_err(CopilotError::Transport)?;
        if status != 200 {
            let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let msg = json
                .get("message")
                .and_then(|m| m.as_str())
                .map_or_else(|| "no message".to_owned(), str::to_owned);
            return Err(CopilotError::github(
                "copilot/metrics/reports/{report}",
                status,
                msg,
            ));
        }
        let envelope: ReportEnvelope =
            serde_json::from_str(&text).map_err(|source| CopilotError::Parse {
                report: report.to_owned(),
                day: day.to_owned(),
                source,
            })?;
        let signed = envelope.download_url().ok_or_else(|| CopilotError::Parse {
            report: report.to_owned(),
            day: day.to_owned(),
            source: serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "report envelope had no download_links",
            )),
        })?;

        // Capture ONLY the host for egress verification, then drop the URL.
        let host = signed
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .map(str::to_owned);
        let bytes = self
            .inner()
            .get(signed)
            .send()
            .await
            .map_err(CopilotError::Transport)?
            .error_for_status()
            .map_err(CopilotError::Transport)?
            .bytes()
            .await
            .map_err(CopilotError::Transport)?
            .to_vec();
        let empty = bytes.is_empty();

        Ok(DownloadedReport { bytes, host, empty })
    }
}
