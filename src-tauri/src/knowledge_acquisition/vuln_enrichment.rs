use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

use crate::knowledge_acquisition::types::KnowledgeResult;
use crate::knowledge_acquisition::KnowledgeAcquisition;

static CVE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"CVE-\d{4}-\d{4,}").unwrap());

/// Enrichment data for a security finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentData {
    pub cve_ids: Vec<String>,
    pub max_cvss_score: Option<f64>,
    pub exploit_available: bool,
    pub exploit_count: usize,
    pub remediation: Option<String>,
    pub references: Vec<String>,
}

impl EnrichmentData {
    /// Format as markdown for inclusion in finding description
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        if !self.cve_ids.is_empty() {
            md.push_str(&format!(
                "**CVEs**: {}\n",
                self.cve_ids.join(", ")
            ));
        }

        if let Some(score) = self.max_cvss_score {
            let severity = match score {
                s if s >= 9.0 => "CRITICAL",
                s if s >= 7.0 => "HIGH",
                s if s >= 4.0 => "MEDIUM",
                _ => "LOW",
            };
            md.push_str(&format!("**Max CVSS**: {score:.1} ({severity})\n"));
        }

        if self.exploit_available {
            md.push_str(&format!(
                "**Exploits available**: {} known exploit(s)\n",
                self.exploit_count
            ));
        }

        if let Some(ref remediation) = self.remediation {
            md.push_str(&format!("**Remediation**: {remediation}\n"));
        }

        if !self.references.is_empty() {
            md.push_str("\n**References**:\n");
            for url in self.references.iter().take(5) {
                md.push_str(&format!("- {url}\n"));
            }
        }

        md
    }
}

/// Extract CVE IDs from text
pub fn extract_cve_ids(text: &str) -> Vec<String> {
    let mut cves: Vec<String> = CVE_RE
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect();
    cves.sort();
    cves.dedup();
    cves
}

/// Enrich a security finding with vulnerability intelligence
///
/// Queries Sploitus for exploit data and extracts CVE/CVSS information
/// from the finding description.
pub async fn enrich_from_description(
    description: &str,
    ka: &KnowledgeAcquisition,
) -> Option<EnrichmentData> {
    let cve_ids = extract_cve_ids(description);

    if cve_ids.is_empty() {
        return None;
    }

    // Query Sploitus with the first CVE ID
    let query = cve_ids.first().unwrap();
    let exploit_results = ka
        .search_vulnerabilities(query)
        .await
        .unwrap_or_default();

    build_enrichment(&cve_ids, &exploit_results)
}

/// Build enrichment data from search results
fn build_enrichment(
    cve_ids: &[String],
    exploit_results: &[KnowledgeResult],
) -> Option<EnrichmentData> {
    if cve_ids.is_empty() && exploit_results.is_empty() {
        return None;
    }

    let max_cvss = exploit_results
        .iter()
        .filter_map(|r| r.metadata.cvss_score)
        .fold(None, |acc: Option<f64>, score| {
            Some(acc.map_or(score, |a| a.max(score)))
        });

    let exploit_count = exploit_results.len();

    let references: Vec<String> = exploit_results
        .iter()
        .filter_map(|r| r.url.clone())
        .take(10)
        .collect();

    // Extract fixed version from exploit results if available
    let remediation = exploit_results
        .iter()
        .find_map(|r| {
            if r.content.contains("Fixed in") {
                r.content
                    .lines()
                    .find(|l| l.contains("Fixed in"))
                    .map(|l| l.trim().to_string())
            } else {
                None
            }
        });

    Some(EnrichmentData {
        cve_ids: cve_ids.to_vec(),
        max_cvss_score: max_cvss,
        exploit_available: exploit_count > 0,
        exploit_count,
        remediation,
        references,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_cve_ids() {
        let text = "Found CVE-2021-44228 and CVE-2024-12345 in the report. Also CVE-2021-44228 again.";
        let cves = extract_cve_ids(text);
        assert_eq!(cves, vec!["CVE-2021-44228", "CVE-2024-12345"]);
    }

    #[test]
    fn test_extract_cve_ids_none() {
        assert!(extract_cve_ids("no vulnerabilities here").is_empty());
    }

    #[test]
    fn test_enrichment_to_markdown() {
        let data = EnrichmentData {
            cve_ids: vec!["CVE-2021-44228".to_string()],
            max_cvss_score: Some(10.0),
            exploit_available: true,
            exploit_count: 3,
            remediation: Some("Upgrade to log4j 2.17.0+".to_string()),
            references: vec!["https://nvd.nist.gov/vuln/detail/CVE-2021-44228".to_string()],
        };
        let md = data.to_markdown();
        assert!(md.contains("CVE-2021-44228"));
        assert!(md.contains("CRITICAL"));
        assert!(md.contains("3 known exploit(s)"));
        assert!(md.contains("Upgrade to log4j"));
    }

    #[test]
    fn test_build_enrichment_empty() {
        assert!(build_enrichment(&[], &[]).is_none());
    }

    #[test]
    fn test_build_enrichment_with_cves_only() {
        let data = build_enrichment(&["CVE-2021-44228".to_string()], &[]).unwrap();
        assert_eq!(data.cve_ids.len(), 1);
        assert!(!data.exploit_available);
        assert_eq!(data.exploit_count, 0);
    }
}
