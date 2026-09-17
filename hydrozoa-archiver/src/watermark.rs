//! Telling the node how far the archive durably reached.
//!
//! This is the only thing the archiver ever says to hydrozoa, and the only reason the two processes
//! are coupled at all. Reading needs no cooperation; this exists purely so the node may delete.

use crate::pass::PassReport;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The report body: the highest durably archived index, per column family.
///
/// Per-family rather than one number, because the journals are independent streams archived at
/// independent rates — `Request:0` can be thousands of entries ahead of `HardAck:3`, and a scalar
/// could only ever carry the minimum, holding retention back to the slowest lane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatermarkReport {
    /// Column-family name → highest index durably in the archive.
    pub watermarks: BTreeMap<String, u64>,
}

/// What the node made of it.
///
/// Field names follow hydrozoa's API, which is camelCase throughout — `effectiveFloor` on the
/// wire. The node owns the convention; this is its client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatermarkResponse {
    /// The floor the node actually adopted per family, after taking the minimum with what
    /// consensus still needs. Lets the archiver see whether it is the binding constraint or the
    /// mesh is — without it, an archiver that is far ahead looks identical to one that is behind.
    #[serde(default)]
    pub effective_floor: BTreeMap<String, u64>,
}

impl WatermarkReport {
    pub fn from_pass(report: &PassReport) -> WatermarkReport {
        WatermarkReport {
            watermarks: report
                .watermarks
                .iter()
                .map(|(cf, index)| (cf.name(), *index))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.watermarks.is_empty()
    }
}

/// Post the report to the node's admin API.
pub fn report(
    client: &reqwest::blocking::Client,
    node_url: &str,
    credentials: Option<(&str, &str)>,
    body: &WatermarkReport,
) -> Result<WatermarkResponse> {
    let url = format!(
        "{}/api/admin/archive/watermark",
        node_url.trim_end_matches('/')
    );
    let mut request = client.post(&url).json(body);
    if let Some((user, password)) = credentials {
        request = request.basic_auth(user, Some(password));
    }

    let response = request
        .send()
        .with_context(|| format!("posting the archive watermark to {url}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        anyhow::bail!("the node rejected the archive watermark ({status}): {body}");
    }
    // A node that answers 200 with no body has accepted the report and told us nothing about the
    // floor it adopted. That is not an error -- the archiver's job is done either way.
    Ok(response.json().unwrap_or(WatermarkResponse {
        effective_floor: BTreeMap::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire names are hydrozoa's, not Rust's. A rename on either side silently produces an
    /// empty `effectiveFloor` rather than an error, so it is pinned here.
    #[test]
    fn the_response_decodes_hydrozoas_camel_case() {
        let json = r#"{"effectiveFloor":{"Block":42,"Request:0":7}}"#;
        let decoded: WatermarkResponse = serde_json::from_str(json).unwrap();
        assert_eq!(decoded.effective_floor.get("Block"), Some(&42));
        assert_eq!(decoded.effective_floor.get("Request:0"), Some(&7));
    }

    /// A node that answers 200 with no floor has accepted the report and said nothing about what
    /// it adopted. Not an error: the archiver's work is done either way.
    #[test]
    fn an_absent_floor_decodes_as_empty() {
        let decoded: WatermarkResponse = serde_json::from_str("{}").unwrap();
        assert!(decoded.effective_floor.is_empty());
    }

    #[test]
    fn the_request_serializes_the_family_names_verbatim() {
        let body = WatermarkReport {
            watermarks: [("Request:0".to_string(), 510941u64)].into_iter().collect(),
        };
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            r#"{"watermarks":{"Request:0":510941}}"#
        );
    }
}
