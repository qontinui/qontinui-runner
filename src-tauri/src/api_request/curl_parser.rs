//! cURL command parser
//!
//! Parses cURL commands copied from browser DevTools into structured request configs.

use super::types::{HttpMethod, ParsedCurl};
use regex::Regex;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CurlParseError {
    #[error("Invalid cURL command: {0}")]
    Invalid(String),
    #[error("Missing URL in cURL command")]
    MissingUrl,
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

/// Parse a cURL command into structured request configuration
///
/// Supports common cURL flags:
/// - `-X/--request METHOD` - HTTP method
/// - `-H/--header "Name: Value"` - Headers
/// - `-d/--data/--data-raw "body"` - Request body
/// - `-u/--user "user:pass"` - Basic auth
/// - URL (quoted or unquoted)
///
/// # Example
/// ```
/// use qontinui_runner::api_request::parse_curl;
///
/// let curl = r#"curl 'https://api.example.com/users' \
///   -H 'Authorization: Bearer token123' \
///   -H 'Content-Type: application/json'"#;
///
/// let parsed = parse_curl(curl).unwrap();
/// assert_eq!(parsed.method, HttpMethod::Get);
/// assert_eq!(parsed.url, "https://api.example.com/users");
/// ```
pub fn parse_curl(curl_command: &str) -> Result<ParsedCurl, CurlParseError> {
    // Normalize the command - remove line continuations and extra whitespace
    let normalized = normalize_command(curl_command);

    if !normalized.trim().to_lowercase().starts_with("curl") {
        return Err(CurlParseError::Invalid(
            "Command must start with 'curl'".to_string(),
        ));
    }

    let mut method = HttpMethod::Get;
    let mut headers: HashMap<String, String> = HashMap::new();
    let mut body: Option<String> = None;
    let url: Option<String>;

    // Parse method (-X or --request)
    let method_re = Regex::new(r#"(?:-X|--request)\s+['""]?(\w+)['""]?"#).unwrap();
    if let Some(caps) = method_re.captures(&normalized) {
        method = parse_method(&caps[1]);
    }

    // Parse headers (-H or --header)
    let header_re = Regex::new(r#"(?:-H|--header)\s+['"](.*?)['"]"#).unwrap();
    for caps in header_re.captures_iter(&normalized) {
        if let Some((name, value)) = parse_header(&caps[1]) {
            headers.insert(name, value);
        }
    }

    // Parse data (-d, --data, --data-raw, --data-binary)
    // Use separate patterns for single and double quoted data to handle nested quotes
    // Single-quoted data (can contain double quotes)
    let data_single_re =
        Regex::new(r#"(?:-d|--data(?:-raw|-binary)?)\s+'([^']*(?:''[^']*)*)'"#).unwrap();
    // Double-quoted data (can contain escaped quotes)
    let data_double_re =
        Regex::new(r#"(?:-d|--data(?:-raw|-binary)?)\s+"([^"\\]*(?:\\.[^"\\]*)*)""#).unwrap();

    if let Some(caps) = data_single_re.captures(&normalized) {
        body = Some(unescape_json(&caps[1]));
        if method == HttpMethod::Get {
            method = HttpMethod::Post;
        }
    } else if let Some(caps) = data_double_re.captures(&normalized) {
        body = Some(unescape_json(&caps[1]));
        if method == HttpMethod::Get {
            method = HttpMethod::Post;
        }
    }

    // Also try to match @- (stdin) data format
    let data_at_re = Regex::new(r#"(?:-d|--data(?:-raw)?)\s+@-"#).unwrap();
    if data_at_re.is_match(&normalized) {
        // Look for a heredoc or subsequent data
        let heredoc_re = Regex::new(r#"<<\s*['""]?(\w+)['""]?\s*([\s\S]*?)\1"#).unwrap();
        if let Some(caps) = heredoc_re.captures(&normalized) {
            body = Some(caps[2].trim().to_string());
            if method == HttpMethod::Get {
                method = HttpMethod::Post;
            }
        }
    }

    // Parse basic auth (-u or --user)
    let user_re = Regex::new(r#"(?:-u|--user)\s+['""]?([^'"\s]+)['""]?"#).unwrap();
    if let Some(caps) = user_re.captures(&normalized) {
        let auth = &caps[1];
        // Base64 encode for Basic auth
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, auth.as_bytes());
        headers.insert("Authorization".to_string(), format!("Basic {}", encoded));
    }

    // Parse URL - try multiple patterns
    url = extract_url(&normalized);

    let url = url.ok_or(CurlParseError::MissingUrl)?;

    // Validate URL
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(CurlParseError::InvalidUrl(format!(
            "URL must start with http:// or https://: {}",
            url
        )));
    }

    // Extract content type from headers
    let content_type = headers
        .get("Content-Type")
        .or_else(|| headers.get("content-type"))
        .cloned();

    Ok(ParsedCurl {
        method,
        url,
        headers,
        body,
        content_type,
    })
}

/// Normalize cURL command by removing line continuations and extra whitespace
fn normalize_command(cmd: &str) -> String {
    // Remove backslash-newline continuations (Unix style)
    let without_continuations = cmd.replace("\\\n", " ").replace("\\\r\n", " ");
    // Remove backtick continuations (PowerShell style)
    let without_backticks = without_continuations
        .replace("`\n", " ")
        .replace("`\r\n", " ");
    // Normalize whitespace
    without_backticks
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Parse HTTP method string to enum
fn parse_method(method_str: &str) -> HttpMethod {
    match method_str.to_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        _ => HttpMethod::Get,
    }
}

/// Parse a header string "Name: Value" into tuple
fn parse_header(header_str: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = header_str.splitn(2, ':').collect();
    if parts.len() == 2 {
        let name = parts[0].trim().to_string();
        let value = parts[1].trim().to_string();
        Some((name, value))
    } else {
        None
    }
}

/// Extract URL from curl command
fn extract_url(cmd: &str) -> Option<String> {
    // Pattern 1: URL in single or double quotes after curl
    let quoted_url_re = Regex::new(r#"curl\s+['"](https?://[^'"]+)['"]"#).unwrap();
    if let Some(caps) = quoted_url_re.captures(cmd) {
        return Some(caps[1].to_string());
    }

    // Pattern 2: URL after all options (look for unquoted URL)
    let unquoted_url_re = Regex::new(r#"\s(https?://[^\s'"]+)"#).unwrap();
    if let Some(caps) = unquoted_url_re.captures(cmd) {
        return Some(caps[1].to_string());
    }

    // Pattern 3: URL in --url option
    let url_option_re = Regex::new(r#"--url\s+['""]?(https?://[^'"\s]+)['""]?"#).unwrap();
    if let Some(caps) = url_option_re.captures(cmd) {
        return Some(caps[1].to_string());
    }

    None
}

/// Unescape JSON string (handle \" and \n)
fn unescape_json(s: &str) -> String {
    s.replace("\\\"", "\"")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_get() {
        let curl = "curl 'https://api.example.com/users'";
        let parsed = parse_curl(curl).unwrap();
        assert_eq!(parsed.method, HttpMethod::Get);
        assert_eq!(parsed.url, "https://api.example.com/users");
    }

    #[test]
    fn test_parse_with_headers() {
        let curl = r#"curl 'https://api.example.com/users' \
            -H 'Authorization: Bearer token123' \
            -H 'Content-Type: application/json'"#;
        let parsed = parse_curl(curl).unwrap();
        assert_eq!(
            parsed.headers.get("Authorization").unwrap(),
            "Bearer token123"
        );
        assert_eq!(parsed.content_type, Some("application/json".to_string()));
    }

    #[test]
    fn test_parse_post_with_data() {
        let curl = r#"curl 'https://api.example.com/users' \
            -X POST \
            -H 'Content-Type: application/json' \
            -d '{"name": "John", "email": "john@example.com"}'"#;
        let parsed = parse_curl(curl).unwrap();
        assert_eq!(parsed.method, HttpMethod::Post);
        assert!(parsed.body.is_some());
        assert!(parsed.body.unwrap().contains("John"));
    }

    #[test]
    fn test_parse_implicit_post() {
        // When -d is provided without -X, method should be POST
        let curl = r#"curl 'https://api.example.com/users' -d '{"name": "John"}'"#;
        let parsed = parse_curl(curl).unwrap();
        assert_eq!(parsed.method, HttpMethod::Post);
    }

    #[test]
    fn test_parse_double_quoted_url() {
        let curl = r#"curl "https://api.example.com/users""#;
        let parsed = parse_curl(curl).unwrap();
        assert_eq!(parsed.url, "https://api.example.com/users");
    }

    #[test]
    fn test_parse_unquoted_url() {
        let curl = "curl https://api.example.com/users";
        let parsed = parse_curl(curl).unwrap();
        assert_eq!(parsed.url, "https://api.example.com/users");
    }

    #[test]
    fn test_parse_full_chrome_curl() {
        let curl = r#"curl 'https://api.example.com/v1/data' \
          -H 'accept: application/json, text/plain, */*' \
          -H 'accept-language: en-US,en;q=0.9' \
          -H 'authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...' \
          -H 'content-type: application/json' \
          -H 'origin: https://app.example.com' \
          -H 'referer: https://app.example.com/' \
          -H 'user-agent: Mozilla/5.0'"#;
        let parsed = parse_curl(curl).unwrap();
        assert_eq!(parsed.method, HttpMethod::Get);
        assert_eq!(parsed.url, "https://api.example.com/v1/data");
        assert!(parsed.headers.contains_key("authorization"));
    }

    #[test]
    fn test_invalid_no_url() {
        let curl = "curl -H 'Authorization: Bearer token'";
        assert!(parse_curl(curl).is_err());
    }

    #[test]
    fn test_invalid_not_curl() {
        let curl = "wget https://example.com";
        assert!(parse_curl(curl).is_err());
    }
}
