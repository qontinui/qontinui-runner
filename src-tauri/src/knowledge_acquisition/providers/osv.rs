use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::knowledge_acquisition::types::{
    KnowledgeDomain, KnowledgeMetadata, KnowledgeResult, SearchProvider,
};

const OSV_QUERY_URL: &str = "https://api.osv.dev/v1/query";
const OSV_BATCH_URL: &str = "https://api.osv.dev/v1/querybatch";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Osv {
    client: Client,
}

/// Supported package ecosystems
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OsvEcosystem {
    #[serde(rename = "npm")]
    Npm,
    #[serde(rename = "PyPI")]
    PyPI,
    #[serde(rename = "crates.io")]
    CratesIo,
    #[serde(rename = "Go")]
    Go,
    #[serde(rename = "Maven")]
    Maven,
    #[serde(rename = "NuGet")]
    NuGet,
}

impl OsvEcosystem {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::PyPI => "PyPI",
            Self::CratesIo => "crates.io",
            Self::Go => "Go",
            Self::Maven => "Maven",
            Self::NuGet => "NuGet",
        }
    }
}

/// Single package query
#[derive(Debug, Clone, Serialize)]
pub struct OsvQueryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<OsvPackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OsvPackage {
    pub name: String,
    pub ecosystem: String,
}

/// Batch query wrapper
#[derive(Debug, Clone, Serialize)]
pub struct OsvBatchQueryRequest {
    pub queries: Vec<OsvQueryRequest>,
}

// --- Response types ---

#[derive(Debug, Clone, Deserialize)]
pub struct OsvQueryResponse {
    #[serde(default)]
    pub vulns: Vec<OsvVulnerability>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvBatchResponse {
    #[serde(default)]
    pub results: Vec<OsvBatchResult>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvBatchResult {
    #[serde(default)]
    pub vulns: Vec<OsvVulnerability>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvVulnerability {
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub severity: Vec<OsvSeverity>,
    #[serde(default)]
    pub affected: Vec<OsvAffected>,
    #[serde(default)]
    pub references: Vec<OsvReference>,
    #[serde(default)]
    pub published: String,
    #[serde(default)]
    pub modified: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvSeverity {
    #[serde(rename = "type", default)]
    pub severity_type: String,
    #[serde(default)]
    pub score: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvAffected {
    #[serde(default)]
    pub package: Option<OsvAffectedPackage>,
    #[serde(default)]
    pub ranges: Vec<OsvRange>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvAffectedPackage {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ecosystem: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvRange {
    #[serde(rename = "type", default)]
    pub range_type: String,
    #[serde(default)]
    pub events: Vec<OsvEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvEvent {
    #[serde(default)]
    pub introduced: Option<String>,
    #[serde(default)]
    pub fixed: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsvReference {
    #[serde(rename = "type", default)]
    pub ref_type: String,
    #[serde(default)]
    pub url: String,
}

impl Osv {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Query vulnerabilities for a specific package version
    pub async fn query_package(
        &self,
        name: &str,
        ecosystem: OsvEcosystem,
        version: &str,
    ) -> Result<Vec<KnowledgeResult>, String> {
        let query = format!(
            "{name}@{version} ({ecosystem})",
            ecosystem = ecosystem.as_str()
        );

        let request = OsvQueryRequest {
            package: Some(OsvPackage {
                name: name.to_string(),
                ecosystem: ecosystem.as_str().to_string(),
            }),
            version: Some(version.to_string()),
        };

        let resp = self
            .client
            .post(OSV_QUERY_URL)
            .json(&request)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("OSV query failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("OSV returned HTTP {}", resp.status()));
        }

        let response: OsvQueryResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse OSV response: {e}"))?;

        Ok(response
            .vulns
            .into_iter()
            .map(|v| vuln_to_result(&v, &query, name, ecosystem, version))
            .collect())
    }

    /// Batch query for multiple packages
    pub async fn query_batch(
        &self,
        packages: &[(String, OsvEcosystem, String)], // (name, ecosystem, version)
    ) -> Result<Vec<(String, Vec<KnowledgeResult>)>, String> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }

        // OSV batch supports up to 1000 queries
        let queries: Vec<OsvQueryRequest> = packages
            .iter()
            .take(1000)
            .map(|(name, ecosystem, version)| OsvQueryRequest {
                package: Some(OsvPackage {
                    name: name.clone(),
                    ecosystem: ecosystem.as_str().to_string(),
                }),
                version: Some(version.clone()),
            })
            .collect();

        let batch_request = OsvBatchQueryRequest { queries };

        let resp = self
            .client
            .post(OSV_BATCH_URL)
            .json(&batch_request)
            .timeout(Duration::from_secs(60)) // longer timeout for batch
            .send()
            .await
            .map_err(|e| format!("OSV batch query failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("OSV batch returned HTTP {}", resp.status()));
        }

        let response: OsvBatchResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse OSV batch response: {e}"))?;

        let mut results = Vec::new();
        for (i, batch_result) in response.results.into_iter().enumerate() {
            if let Some((name, ecosystem, version)) = packages.get(i) {
                let query = format!("{name}@{version}");
                let pkg_results: Vec<KnowledgeResult> = batch_result
                    .vulns
                    .into_iter()
                    .map(|v| vuln_to_result(&v, &query, name, *ecosystem, version))
                    .collect();
                if !pkg_results.is_empty() {
                    results.push((name.clone(), pkg_results));
                }
            }
        }

        Ok(results)
    }
}

/// Convert an OSV vulnerability to a KnowledgeResult
fn vuln_to_result(
    vuln: &OsvVulnerability,
    query: &str,
    package_name: &str,
    ecosystem: OsvEcosystem,
    package_version: &str,
) -> KnowledgeResult {
    let cvss_score = extract_cvss_score(vuln);
    let fixed_version = extract_fixed_version(vuln);

    let mut content = format!("## {} — {}\n\n", vuln.id, vuln.summary);

    if !vuln.details.is_empty() {
        content.push_str(&vuln.details);
        content.push_str("\n\n");
    }

    if let Some(score) = cvss_score {
        let severity = match score {
            s if s >= 9.0 => "CRITICAL",
            s if s >= 7.0 => "HIGH",
            s if s >= 4.0 => "MEDIUM",
            _ => "LOW",
        };
        content.push_str(&format!("**CVSS**: {score:.1} ({severity})\n"));
    }

    if let Some(ref fixed) = fixed_version {
        content.push_str(&format!("**Fixed in**: {fixed}\n"));
    }

    content.push_str(&format!(
        "**Package**: {package_name} ({ecosystem})\n**Affected version**: {package_version}\n",
        ecosystem = ecosystem.as_str()
    ));

    if !vuln.references.is_empty() {
        content.push_str("\n**References**:\n");
        for r in vuln.references.iter().take(5) {
            content.push_str(&format!("- [{}]({})\n", r.ref_type, r.url));
        }
    }

    KnowledgeResult {
        provider: SearchProvider::OsvDev,
        query: query.to_string(),
        title: format!("{} — {}", vuln.id, vuln.summary),
        content,
        url: vuln.references.first().map(|r| r.url.clone()),
        relevance_score: Some(1.0), // structured data = high relevance
        metadata: KnowledgeMetadata {
            domain: Some(KnowledgeDomain::VulnerabilityIntelligence),
            cvss_score,
            cve_id: Some(vuln.id.clone()),
            ecosystem: Some(ecosystem.as_str().to_string()),
            package_name: Some(package_name.to_string()),
            package_version: Some(package_version.to_string()),
        },
    }
}

/// Parse CVSS numeric score from severity vector string
fn extract_cvss_score(vuln: &OsvVulnerability) -> Option<f64> {
    for sev in &vuln.severity {
        if sev.severity_type.contains("CVSS") {
            // CVSS vector format: "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"
            // The numeric score is sometimes in a separate field, or we estimate from vector
            // Try parsing as a plain number first
            if let Ok(score) = sev.score.parse::<f64>() {
                return Some(score);
            }
        }
    }
    None
}

/// Extract the earliest fixed version from affected ranges
fn extract_fixed_version(vuln: &OsvVulnerability) -> Option<String> {
    for affected in &vuln.affected {
        for range in &affected.ranges {
            for event in &range.events {
                if let Some(ref fixed) = event.fixed {
                    return Some(fixed.clone());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_osv_response() {
        let json = r#"{
            "vulns": [{
                "id": "GHSA-xxxx-yyyy-zzzz",
                "summary": "Prototype pollution in lodash",
                "details": "lodash before 4.17.21 is vulnerable to prototype pollution.",
                "severity": [{"type": "CVSS_V3", "score": "7.5"}],
                "affected": [{
                    "package": {"name": "lodash", "ecosystem": "npm"},
                    "ranges": [{"type": "SEMVER", "events": [
                        {"introduced": "0"},
                        {"fixed": "4.17.21"}
                    ]}]
                }],
                "references": [{"type": "ADVISORY", "url": "https://github.com/advisories/GHSA-xxxx"}],
                "published": "2021-01-01T00:00:00Z",
                "modified": "2021-06-01T00:00:00Z"
            }]
        }"#;

        let response: OsvQueryResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.vulns.len(), 1);

        let vuln = &response.vulns[0];
        assert_eq!(vuln.id, "GHSA-xxxx-yyyy-zzzz");
        assert_eq!(extract_cvss_score(vuln), Some(7.5));
        assert_eq!(extract_fixed_version(vuln), Some("4.17.21".to_string()));
    }

    #[test]
    fn test_vuln_to_result() {
        let vuln = OsvVulnerability {
            id: "CVE-2021-44228".to_string(),
            summary: "Log4Shell".to_string(),
            details: "Remote code execution via JNDI.".to_string(),
            severity: vec![OsvSeverity {
                severity_type: "CVSS_V3".to_string(),
                score: "10.0".to_string(),
            }],
            affected: vec![],
            references: vec![OsvReference {
                ref_type: "ADVISORY".to_string(),
                url: "https://nvd.nist.gov/vuln/detail/CVE-2021-44228".to_string(),
            }],
            published: "2021-12-10".to_string(),
            modified: "2021-12-15".to_string(),
        };

        let result = vuln_to_result(
            &vuln,
            "log4j@2.14.1",
            "log4j",
            OsvEcosystem::Maven,
            "2.14.1",
        );
        assert_eq!(result.provider, SearchProvider::OsvDev);
        assert!(result.content.contains("CRITICAL"));
        assert!(result.content.contains("10.0"));
        assert_eq!(result.metadata.cvss_score, Some(10.0));
        assert_eq!(result.metadata.cve_id.as_deref(), Some("CVE-2021-44228"));
    }

    #[test]
    fn test_empty_response() {
        let json = r#"{"vulns": []}"#;
        let response: OsvQueryResponse = serde_json::from_str(json).unwrap();
        assert!(response.vulns.is_empty());
    }

    #[test]
    fn test_batch_response() {
        let json = r#"{
            "results": [
                {"vulns": [{"id": "CVE-1", "summary": "Vuln 1", "details": "", "severity": [], "affected": [], "references": [], "published": "", "modified": ""}]},
                {"vulns": []}
            ]
        }"#;
        let response: OsvBatchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].vulns.len(), 1);
        assert!(response.results[1].vulns.is_empty());
    }
}
