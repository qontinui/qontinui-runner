#![allow(dead_code)]

/// Result of running an AI prompt
#[derive(Debug, Clone)]
pub struct AiResponse {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    /// Input tokens consumed (available for API providers only)
    pub input_tokens: Option<u64>,
    /// Output tokens generated (available for API providers only)
    pub output_tokens: Option<u64>,
}

impl AiResponse {
    /// Create a successful response with token information
    pub fn success_with_tokens(output: String, input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            success: true,
            output,
            error: None,
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
        }
    }

    /// Create a successful response without token information (for CLI providers)
    pub fn success(output: String) -> Self {
        Self {
            success: true,
            output,
            error: None,
            input_tokens: None,
            output_tokens: None,
        }
    }

    /// Create an error response
    pub fn error(message: String) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(message),
            input_tokens: None,
            output_tokens: None,
        }
    }

    /// Create an error response with partial output
    pub fn error_with_output(output: String, message: String) -> Self {
        Self {
            success: false,
            output,
            error: Some(message),
            input_tokens: None,
            output_tokens: None,
        }
    }

    /// Get total tokens (input + output)
    pub fn total_tokens(&self) -> Option<u64> {
        match (self.input_tokens, self.output_tokens) {
            (Some(input), Some(output)) => Some(input + output),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ai_response_success() {
        let response = AiResponse::success("Hello!".to_string());
        assert!(response.success);
        assert_eq!(response.output, "Hello!");
        assert!(response.error.is_none());
        assert!(response.input_tokens.is_none());
        assert!(response.output_tokens.is_none());
        assert!(response.total_tokens().is_none());
    }

    #[test]
    fn test_ai_response_success_with_tokens() {
        let response = AiResponse::success_with_tokens("Hello!".to_string(), 100, 50);
        assert!(response.success);
        assert_eq!(response.output, "Hello!");
        assert!(response.error.is_none());
        assert_eq!(response.input_tokens, Some(100));
        assert_eq!(response.output_tokens, Some(50));
        assert_eq!(response.total_tokens(), Some(150));
    }

    #[test]
    fn test_ai_response_failure() {
        let response = AiResponse::error("Connection failed".to_string());
        assert!(!response.success);
        assert!(response.error.is_some());
        assert_eq!(response.error, Some("Connection failed".to_string()));
        assert!(response.input_tokens.is_none());
        assert!(response.output_tokens.is_none());
    }

    #[test]
    fn test_ai_response_failure_with_output() {
        let response = AiResponse::error_with_output(
            "partial output".to_string(),
            "Error occurred".to_string(),
        );
        assert!(!response.success);
        assert_eq!(response.output, "partial output");
        assert_eq!(response.error, Some("Error occurred".to_string()));
    }
}
