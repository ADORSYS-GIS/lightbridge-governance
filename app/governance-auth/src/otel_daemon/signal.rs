//! OTLP signals supported by the daemon, independent of Copilot's file drain.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Signal {
    Logs,
    Metrics,
    Traces,
}

impl Signal {
    pub fn path(self) -> &'static str {
        match self {
            Self::Logs => "/v1/logs",
            Self::Metrics => "/v1/metrics",
            Self::Traces => "/v1/traces",
        }
    }

    pub fn json_key(self) -> &'static str {
        match self {
            Self::Logs => "resourceLogs",
            Self::Metrics => "resourceMetrics",
            Self::Traces => "resourceSpans",
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        [Self::Logs, Self::Metrics, Self::Traces]
            .into_iter()
            .find(|signal| path.ends_with(signal.path()))
    }
}

impl std::fmt::Display for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Logs => "logs",
            Self::Metrics => "metrics",
            Self::Traces => "traces",
        })
    }
}
