//! The raw-report archive sink: S3 in production, a local directory in dev.
//!
//! `sync_day`'s archive closure is synchronous (the connector API takes
//! `impl Fn(&str, &[u8])`), so the S3 calls here run via
//! `Handle::current().block_on` on the multi-thread runtime this binary starts
//! with. That is fine for a daily CronJob run of sequential writes; if the
//! collector ever goes concurrent, the connector's archive signature is where
//! the async boundary belongs.
//!
//! Object layout follows the plan: `copilot-governance/raw/` prefix on S3 over
//! the connector's relative key (`org=…/day=…/{report}.ndjson`). The S3
//! endpoint is Hetzner Object Storage (Ceph-RGW), path-style, same as the
//! LibreChat/CNPG-bootstrap precedent.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Prefix under which the raw archive lives in the shared S3 bucket.
const S3_PREFIX: &str = "copilot-governance/raw/";
/// Default Hetzner Object Storage endpoint (see chart values; overridable).
const S3_ENDPOINT: &str = "https://nbg1.your-objectstorage.com";
/// Ceph-RGW region; any value is accepted by the endpoint.
const S3_REGION: &str = "us-east-1";

/// Where raw report NDJSON is archived.
#[derive(Debug, Clone)]
pub enum Archive {
    /// Production sink: the shared `ssegning-k8s-state` bucket (or
    /// `S3_BUCKET`), creds from `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`.
    S3 {
        client: aws_sdk_s3::Client,
        bucket: String,
    },
    /// Dev sink: a plain directory, `RAW_DIR`.
    Local { dir: PathBuf },
}

impl Archive {
    /// Build the configured sink. `None` means no sink is configured; the
    /// caller must fail loudly rather than silently skip archiving.
    pub async fn from_env() -> Result<Option<Self>> {
        if std::env::var("AWS_ACCESS_KEY_ID").is_ok()
            && std::env::var("AWS_SECRET_ACCESS_KEY").is_ok()
        {
            return Self::s3_from_env().await.map(Some);
        }
        if let Ok(dir) = std::env::var("RAW_DIR") {
            return Ok(Some(Self::Local {
                dir: PathBuf::from(dir),
            }));
        }
        Ok(None)
    }

    async fn s3_from_env() -> Result<Self> {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| S3_REGION.to_owned());
        let endpoint = std::env::var("AWS_ENDPOINT_URL").unwrap_or_else(|_| S3_ENDPOINT.to_owned());
        let force_path_style = std::env::var("AWS_FORCE_PATH_STYLE").map_or(true, |v| v == "true");
        let bucket = std::env::var("S3_BUCKET").unwrap_or_else(|_| "ssegning-k8s-state".to_owned());

        // Creds are read explicitly (from_env already demanded the two env
        // vars) so no credential-provider chain -- and no IMDS probing -- is
        // involved; the vars come from the chart's ExternalSecret. `new` (not
        // `from_keys`) because the hardcoded-credentials feature is off.
        let access_key = std::env::var("AWS_ACCESS_KEY_ID").context("AWS_ACCESS_KEY_ID")?;
        let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY").context("AWS_SECRET_ACCESS_KEY")?;
        let creds = aws_sdk_s3::config::Credentials::new(access_key, secret_key, None, None, "env");

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region))
            .endpoint_url(endpoint)
            .credentials_provider(creds)
            .load()
            .await;
        let s3_config = aws_sdk_s3::config::Builder::from(&config)
            .force_path_style(force_path_style)
            .build();
        Ok(Self::S3 {
            client: aws_sdk_s3::Client::from_conf(s3_config),
            bucket,
        })
    }

    /// Archive the raw payload. The connector hands us its relative key; the
    /// sink maps it to its own layout.
    pub fn write(&self, key: &str, bytes: &[u8]) -> Result<()> {
        match self {
            Self::S3 { client, bucket } => {
                let full = format!("{S3_PREFIX}{key}");
                let fut = client
                    .put_object()
                    .bucket(bucket)
                    .key(full)
                    .body(aws_sdk_s3::primitives::ByteStream::from(bytes.to_vec()))
                    .send();
                tokio::runtime::Handle::current()
                    .block_on(fut)
                    .with_context(|| format!("s3 put {key}"))?;
            }
            Self::Local { dir } => {
                let path = dir.join(key);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, bytes)?;
            }
        }
        Ok(())
    }

    /// Read one archived report back, for `replay`.
    pub async fn read(&self, key: &str) -> Result<Vec<u8>> {
        match self {
            Self::S3 { client, bucket } => {
                let full = format!("{S3_PREFIX}{key}");
                let out = client
                    .get_object()
                    .bucket(bucket)
                    .key(full)
                    .send()
                    .await
                    .with_context(|| format!("s3 get {key}"))?;
                let body = out
                    .body
                    .collect()
                    .await
                    .with_context(|| format!("s3 get body {key}"))?;
                Ok(body.into_bytes().to_vec())
            }
            Self::Local { dir } => Ok(std::fs::read(dir.join(key))?),
        }
    }

    /// All archived report keys for one day of `org`, relative to the sink.
    pub async fn list_day(&self, org: &str, day: &str) -> Result<Vec<String>> {
        let prefix = format!("org={org}/day={day}/");
        match self {
            Self::S3 { client, bucket } => {
                let full_prefix = format!("{S3_PREFIX}{prefix}");
                let mut out = Vec::new();
                let mut paginator = client
                    .list_objects_v2()
                    .bucket(bucket)
                    .prefix(full_prefix)
                    .into_paginator()
                    .send();
                while let Some(page) = paginator.next().await {
                    let page = page.with_context(|| format!("s3 list {prefix}"))?;
                    for obj in page.contents() {
                        if let Some(key) = obj.key().and_then(|k| k.strip_prefix(S3_PREFIX)) {
                            out.push(key.to_owned());
                        }
                    }
                }
                Ok(out)
            }
            Self::Local { dir } => {
                let day_dir = dir.join(&prefix);
                let mut out = Vec::new();
                if day_dir.is_dir() {
                    for entry in std::fs::read_dir(&day_dir)? {
                        let entry = entry?;
                        if entry.path().is_file() {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            out.push(format!("{prefix}{name}"));
                        }
                    }
                }
                Ok(out)
            }
        }
    }
}
