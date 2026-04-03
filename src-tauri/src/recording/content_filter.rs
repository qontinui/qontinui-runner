//! Content filtering for the activity timeline.
//!
//! Provides PII scrubbing (emails, credit cards, SSNs, API keys) and
//! sensitive application detection (password managers, incognito windows)
//! before captured text is stored in the activity timeline.
//!
//! Inspired by screenpipe's privacy-first approach to continuous capture.

use regex::Regex;

/// A single PII detection pattern with its replacement text.
struct PiiPattern {
    name: &'static str,
    regex: Regex,
    replacement: &'static str,
}

/// Content filter that scrubs PII and detects sensitive applications.
pub struct ContentFilter {
    pii_patterns: Vec<PiiPattern>,
    sensitive_apps: Vec<String>,
}

/// Result of filtering text content.
pub struct FilterResult {
    /// The scrubbed text (PII replaced with placeholders).
    pub text: String,
    /// Number of PII matches found and replaced.
    pub pii_matches: usize,
}

impl ContentFilter {
    /// Create a new content filter with default PII patterns and sensitive app list.
    pub fn new() -> Self {
        Self {
            pii_patterns: Self::default_pii_patterns(),
            sensitive_apps: Self::default_sensitive_apps(),
        }
    }

    /// Create a content filter with custom sensitive app patterns.
    pub fn with_sensitive_apps(sensitive_apps: Vec<String>) -> Self {
        Self {
            pii_patterns: Self::default_pii_patterns(),
            sensitive_apps,
        }
    }

    /// Check if a window/app should be skipped entirely (no capture).
    pub fn should_skip(&self, app_name: &str, window_title: &str) -> bool {
        let app_lower = app_name.to_lowercase();
        let title_lower = window_title.to_lowercase();

        // Check sensitive apps
        for pattern in &self.sensitive_apps {
            let pattern_lower = pattern.to_lowercase();
            if app_lower.contains(&pattern_lower) || title_lower.contains(&pattern_lower) {
                return true;
            }
        }

        // Detect incognito/private browsing windows
        if title_lower.contains("incognito")
            || title_lower.contains("private browsing")
            || title_lower.contains("inprivate")
        {
            return true;
        }

        false
    }

    /// Scrub PII from text, replacing matches with placeholders like [EMAIL], [SSN], etc.
    pub fn scrub(&self, text: &str) -> FilterResult {
        let mut result = text.to_string();
        let mut total_matches = 0;

        for pattern in &self.pii_patterns {
            let matches: Vec<_> = pattern.regex.find_iter(&result).collect();
            total_matches += matches.len();
            result = pattern
                .regex
                .replace_all(&result, pattern.replacement)
                .to_string();
        }

        FilterResult {
            text: result,
            pii_matches: total_matches,
        }
    }

    /// Convenience: scrub text and return None if the window should be skipped.
    pub fn filter(&self, text: &str, app_name: &str, window_title: &str) -> Option<FilterResult> {
        if self.should_skip(app_name, window_title) {
            return None;
        }
        Some(self.scrub(text))
    }

    fn default_pii_patterns() -> Vec<PiiPattern> {
        vec![
            PiiPattern {
                name: "email",
                regex: Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap(),
                replacement: "[EMAIL]",
            },
            PiiPattern {
                name: "credit_card",
                // Matches card formats starting with 3-6 (Amex/Visa/MC/Discover) to reduce false positives
                regex: Regex::new(r"\b[3-6]\d{3}[\s\-]?\d{4}[\s\-]?\d{4}[\s\-]?\d{4}\b").unwrap(),
                replacement: "[CREDIT_CARD]",
            },
            PiiPattern {
                name: "ssn",
                regex: Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(),
                replacement: "[SSN]",
            },
            PiiPattern {
                name: "api_key_bearer",
                regex: Regex::new(r"(?i)bearer\s+[a-zA-Z0-9\-._~+/]+=*").unwrap(),
                replacement: "[BEARER_TOKEN]",
            },
            PiiPattern {
                name: "api_key_sk",
                // OpenAI-style keys: sk-...
                regex: Regex::new(r"\bsk-[a-zA-Z0-9]{20,}").unwrap(),
                replacement: "[API_KEY]",
            },
            PiiPattern {
                name: "api_key_ghp",
                // GitHub personal access tokens
                regex: Regex::new(r"\bghp_[a-zA-Z0-9]{36,}").unwrap(),
                replacement: "[GITHUB_TOKEN]",
            },
            PiiPattern {
                name: "api_key_generic",
                // Generic "key" or "token" followed by = or : and a long alphanumeric value
                regex: Regex::new(
                    r"(?i)(?:api[_-]?key|api[_-]?secret|access[_-]?token|auth[_-]?token)\s*[=:]\s*[a-zA-Z0-9\-._~+/]{16,}",
                )
                .unwrap(),
                replacement: "[REDACTED_KEY]",
            },
        ]
    }

    fn default_sensitive_apps() -> Vec<String> {
        vec![
            "1Password".to_string(),
            "KeePass".to_string(),
            "KeePassXC".to_string(),
            "Bitwarden".to_string(),
            "LastPass".to_string(),
            "Dashlane".to_string(),
            "Enpass".to_string(),
            "Keeper".to_string(),
            "NordPass".to_string(),
            "Credential Manager".to_string(),
            "Keychain Access".to_string(),
        ]
    }
}

impl Default for ContentFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scrub_email() {
        let filter = ContentFilter::new();
        let result = filter.scrub("Contact user@example.com for details");
        assert_eq!(result.text, "Contact [EMAIL] for details");
        assert_eq!(result.pii_matches, 1);
    }

    #[test]
    fn test_scrub_credit_card() {
        let filter = ContentFilter::new();
        let result = filter.scrub("Card: 4111-1111-1111-1111");
        assert_eq!(result.text, "Card: [CREDIT_CARD]");
        assert_eq!(result.pii_matches, 1);
    }

    #[test]
    fn test_scrub_ssn() {
        let filter = ContentFilter::new();
        let result = filter.scrub("SSN: 123-45-6789");
        assert_eq!(result.text, "SSN: [SSN]");
        assert_eq!(result.pii_matches, 1);
    }

    #[test]
    fn test_scrub_api_keys() {
        let filter = ContentFilter::new();
        let result = filter.scrub("Key: sk-abc123def456ghi789jkl012mno");
        assert!(result.text.contains("[API_KEY]"));
        assert_eq!(result.pii_matches, 1);
    }

    #[test]
    fn test_scrub_bearer_token() {
        let filter = ContentFilter::new();
        let result = filter.scrub("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9");
        assert!(result.text.contains("[BEARER_TOKEN]"));
    }

    #[test]
    fn test_scrub_multiple() {
        let filter = ContentFilter::new();
        let result = filter.scrub("Email: a@b.com, SSN: 111-22-3333");
        assert_eq!(result.text, "Email: [EMAIL], SSN: [SSN]");
        assert_eq!(result.pii_matches, 2);
    }

    #[test]
    fn test_scrub_no_pii() {
        let filter = ContentFilter::new();
        let result = filter.scrub("Hello world, no sensitive data here");
        assert_eq!(result.text, "Hello world, no sensitive data here");
        assert_eq!(result.pii_matches, 0);
    }

    #[test]
    fn test_should_skip_password_manager() {
        let filter = ContentFilter::new();
        assert!(filter.should_skip("1Password 7", "Vault"));
        assert!(filter.should_skip("KeePassXC", "Database"));
        assert!(filter.should_skip("Bitwarden", "My Vault"));
    }

    #[test]
    fn test_should_skip_incognito() {
        let filter = ContentFilter::new();
        assert!(filter.should_skip("Chrome", "New Incognito Tab - Google Chrome"));
        assert!(filter.should_skip("Firefox", "Private Browsing"));
        assert!(filter.should_skip("Edge", "InPrivate - Microsoft Edge"));
    }

    #[test]
    fn test_should_not_skip_normal() {
        let filter = ContentFilter::new();
        assert!(!filter.should_skip("VS Code", "main.rs - qontinui-runner"));
        assert!(!filter.should_skip("Chrome", "GitHub - Pull Requests"));
    }

    #[test]
    fn test_filter_skips_sensitive() {
        let filter = ContentFilter::new();
        assert!(filter.filter("some text", "1Password", "Vault").is_none());
    }

    #[test]
    fn test_filter_scrubs_normal() {
        let filter = ContentFilter::new();
        let result = filter.filter("Email: a@b.com", "VS Code", "main.rs");
        assert!(result.is_some());
        assert_eq!(result.unwrap().text, "Email: [EMAIL]");
    }
}
