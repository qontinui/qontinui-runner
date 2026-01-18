//! Security vulnerability patterns
//!
//! Defines regex patterns for detecting common security vulnerabilities across multiple languages.

/// A security vulnerability pattern
#[derive(Debug, Clone)]
pub struct SecurityPattern {
    /// Unique identifier for this pattern
    pub id: &'static str,
    /// Human-readable name
    pub name: &'static str,
    /// Description of the vulnerability
    pub description: &'static str,
    /// Severity level: "critical", "high", "medium", "low"
    pub severity: &'static str,
    /// Regex pattern to match
    pub pattern: &'static str,
    /// Languages this pattern applies to: "python", "typescript", "rust", "all"
    pub languages: &'static [&'static str],
}

// ============================================================================
// Hardcoded Secrets Patterns
// ============================================================================

/// AWS access key pattern
pub const AWS_ACCESS_KEY: SecurityPattern = SecurityPattern {
    id: "SEC001",
    name: "AWS Access Key",
    description: "Hardcoded AWS access key ID detected",
    severity: "critical",
    pattern: r#"(?i)(AKIA[0-9A-Z]{16})"#,
    languages: &["all"],
};

/// AWS secret key pattern
pub const AWS_SECRET_KEY: SecurityPattern = SecurityPattern {
    id: "SEC002",
    name: "AWS Secret Key",
    description: "Potential AWS secret access key detected",
    severity: "critical",
    pattern: r#"(?i)(aws_secret_access_key|aws_secret_key)\s*[=:]\s*['\"]?([A-Za-z0-9/+=]{40})['\"]?"#,
    languages: &["all"],
};

/// Generic API key pattern
pub const GENERIC_API_KEY: SecurityPattern = SecurityPattern {
    id: "SEC003",
    name: "Generic API Key",
    description: "Potential hardcoded API key detected",
    severity: "high",
    pattern: r#"(?i)(api[_-]?key|apikey)\s*[=:]\s*['\"]([a-zA-Z0-9_\-]{20,})['\"]"#,
    languages: &["all"],
};

/// Generic secret pattern
pub const GENERIC_SECRET: SecurityPattern = SecurityPattern {
    id: "SEC004",
    name: "Generic Secret",
    description: "Potential hardcoded secret detected",
    severity: "high",
    pattern: r#"(?i)(secret|password|passwd|pwd|token)\s*[=:]\s*['\"]([^'\"]{8,})['\"]"#,
    languages: &["all"],
};

/// Private key pattern
pub const PRIVATE_KEY: SecurityPattern = SecurityPattern {
    id: "SEC005",
    name: "Private Key",
    description: "Private key content detected in source code",
    severity: "critical",
    pattern: r#"-----BEGIN\s+(RSA\s+)?PRIVATE\s+KEY-----"#,
    languages: &["all"],
};

/// GitHub token pattern
pub const GITHUB_TOKEN: SecurityPattern = SecurityPattern {
    id: "SEC006",
    name: "GitHub Token",
    description: "Potential GitHub token detected",
    severity: "critical",
    pattern: r#"(ghp_[a-zA-Z0-9]{36}|github_pat_[a-zA-Z0-9]{22}_[a-zA-Z0-9]{59})"#,
    languages: &["all"],
};

/// Slack webhook/token pattern
pub const SLACK_TOKEN: SecurityPattern = SecurityPattern {
    id: "SEC007",
    name: "Slack Token",
    description: "Potential Slack token or webhook URL detected",
    severity: "high",
    pattern: r#"(xox[baprs]-[0-9a-zA-Z]{10,48}|https://hooks\.slack\.com/services/[A-Z0-9]+/[A-Z0-9]+/[a-zA-Z0-9]+)"#,
    languages: &["all"],
};

/// JWT secret pattern
pub const JWT_SECRET: SecurityPattern = SecurityPattern {
    id: "SEC008",
    name: "JWT Secret",
    description: "Potential hardcoded JWT secret detected",
    severity: "high",
    pattern: r#"(?i)(jwt[_-]?secret|jwt[_-]?key)\s*[=:]\s*['\"]([^'\"]{16,})['\"]"#,
    languages: &["all"],
};

// ============================================================================
// SQL Injection Patterns
// ============================================================================

/// SQL injection via string concatenation
pub const SQL_INJECTION_CONCAT: SecurityPattern = SecurityPattern {
    id: "SEC010",
    name: "SQL Injection (String Concatenation)",
    description: "Potential SQL injection via string concatenation",
    severity: "critical",
    pattern: r#"(?i)(execute|cursor\.execute|query|rawQuery|raw)\s*\(\s*['"](SELECT|INSERT|UPDATE|DELETE|DROP)[^'"]*['"]\s*\+|(?i)f['\"]SELECT|f['\"]INSERT|f['\"]UPDATE|f['\"]DELETE"#,
    languages: &["python", "typescript"],
};

/// SQL injection via format strings (Python)
pub const SQL_INJECTION_FORMAT_PY: SecurityPattern = SecurityPattern {
    id: "SEC011",
    name: "SQL Injection (Format String)",
    description: "Potential SQL injection via Python format strings",
    severity: "critical",
    pattern: r#"(?i)(execute|cursor\.execute)\s*\(\s*['\"][^'\"]*%s[^'\"]*['\"]|\.format\([^)]*\)\s*\)"#,
    languages: &["python"],
};

/// SQL injection via template literals (TypeScript)
pub const SQL_INJECTION_TEMPLATE: SecurityPattern = SecurityPattern {
    id: "SEC012",
    name: "SQL Injection (Template Literal)",
    description: "Potential SQL injection via template literals",
    severity: "critical",
    pattern: r#"(?i)(query|execute|rawQuery)\s*\(\s*`[^`]*(SELECT|INSERT|UPDATE|DELETE)[^`]*\$\{[^}]+\}"#,
    languages: &["typescript"],
};

// ============================================================================
// Command Injection Patterns
// ============================================================================

/// Command injection via os.system (Python)
pub const CMD_INJECTION_SYSTEM: SecurityPattern = SecurityPattern {
    id: "SEC020",
    name: "Command Injection (os.system)",
    description: "Potential command injection via os.system()",
    severity: "critical",
    pattern: r#"(?i)os\.system\s*\([^)]*(\+|format|f['\"]|\%)"#,
    languages: &["python"],
};

/// Command injection via subprocess with shell=True
pub const CMD_INJECTION_SUBPROCESS: SecurityPattern = SecurityPattern {
    id: "SEC021",
    name: "Command Injection (subprocess shell=True)",
    description: "Dangerous subprocess call with shell=True",
    severity: "high",
    pattern: r#"(?i)subprocess\.(call|run|Popen)\s*\([^)]*shell\s*=\s*True"#,
    languages: &["python"],
};

/// Command injection via eval
pub const CMD_INJECTION_EVAL: SecurityPattern = SecurityPattern {
    id: "SEC022",
    name: "Dangerous eval()",
    description: "Use of eval() with potentially untrusted input",
    severity: "critical",
    pattern: r#"(?i)\beval\s*\(\s*[^'\"\)][^)]*\)"#,
    languages: &["python", "typescript"],
};

/// Command injection via exec (TypeScript)
pub const CMD_INJECTION_EXEC: SecurityPattern = SecurityPattern {
    id: "SEC023",
    name: "Command Injection (child_process.exec)",
    description: "Potential command injection via exec()",
    severity: "critical",
    pattern: r#"(?i)(exec|execSync|spawn)\s*\([^)]*(\+|`\$\{)"#,
    languages: &["typescript"],
};

/// Unsafe command in Rust
pub const CMD_INJECTION_RUST: SecurityPattern = SecurityPattern {
    id: "SEC024",
    name: "Command Injection (Rust)",
    description: "Potential command injection via Command::new with shell",
    severity: "high",
    pattern: r#"Command::new\s*\(\s*['\"](?:sh|bash|cmd|powershell)['\"]"#,
    languages: &["rust"],
};

// ============================================================================
// Path Traversal Patterns
// ============================================================================

/// Path traversal via string concatenation
pub const PATH_TRAVERSAL_CONCAT: SecurityPattern = SecurityPattern {
    id: "SEC030",
    name: "Path Traversal",
    description: "Potential path traversal vulnerability",
    severity: "high",
    pattern: r#"(?i)(open|read|write|Path|join)\s*\([^)]*(\+|format|f['\"])[^)]*\.\.\/"#,
    languages: &["python", "typescript"],
};

/// Unvalidated file path
pub const PATH_UNVALIDATED: SecurityPattern = SecurityPattern {
    id: "SEC031",
    name: "Unvalidated File Path",
    description: "File operation with potentially unvalidated path from user input",
    severity: "medium",
    pattern: r#"(?i)(open|readFile|writeFile|Path\.join)\s*\(\s*(req\.|request\.|params\.|query\.)"#,
    languages: &["python", "typescript"],
};

// ============================================================================
// Insecure Deserialization Patterns
// ============================================================================

/// Pickle deserialization (Python)
pub const INSECURE_PICKLE: SecurityPattern = SecurityPattern {
    id: "SEC040",
    name: "Insecure Pickle Deserialization",
    description: "Unsafe pickle deserialization can execute arbitrary code",
    severity: "critical",
    pattern: r#"(?i)pickle\.(load|loads)\s*\("#,
    languages: &["python"],
};

/// YAML unsafe load (Python)
pub const INSECURE_YAML: SecurityPattern = SecurityPattern {
    id: "SEC041",
    name: "Insecure YAML Load",
    description: "Unsafe YAML loading can execute arbitrary code",
    severity: "critical",
    pattern: r#"(?i)yaml\.(load|unsafe_load)\s*\([^)]*(?!Loader|safe)"#,
    languages: &["python"],
};

/// Eval-based deserialization
pub const INSECURE_DESERIALIZE_EVAL: SecurityPattern = SecurityPattern {
    id: "SEC042",
    name: "Insecure Deserialization (eval)",
    description: "Using eval for deserialization is dangerous",
    severity: "critical",
    pattern: r#"(?i)JSON\.parse\s*\([^)]*eval"#,
    languages: &["typescript"],
};

// ============================================================================
// Weak Cryptography Patterns
// ============================================================================

/// MD5 usage
pub const WEAK_CRYPTO_MD5: SecurityPattern = SecurityPattern {
    id: "SEC050",
    name: "Weak Hash (MD5)",
    description: "MD5 is cryptographically broken, use SHA-256 or better",
    severity: "medium",
    pattern: r#"(?i)(md5|hashlib\.md5|crypto\.createHash\s*\(\s*['\"]md5['\"])"#,
    languages: &["python", "typescript"],
};

/// SHA1 usage
pub const WEAK_CRYPTO_SHA1: SecurityPattern = SecurityPattern {
    id: "SEC051",
    name: "Weak Hash (SHA1)",
    description: "SHA1 is deprecated for security use, use SHA-256 or better",
    severity: "medium",
    pattern: r#"(?i)(sha1|hashlib\.sha1|crypto\.createHash\s*\(\s*['\"]sha1['\"])"#,
    languages: &["python", "typescript"],
};

/// DES/3DES usage
pub const WEAK_CRYPTO_DES: SecurityPattern = SecurityPattern {
    id: "SEC052",
    name: "Weak Encryption (DES)",
    description: "DES/3DES are deprecated, use AES-256",
    severity: "medium",
    pattern: r#"(?i)(DES|3DES|TDES|TripleDES)"#,
    languages: &["all"],
};

/// Hardcoded IV/nonce
pub const WEAK_CRYPTO_IV: SecurityPattern = SecurityPattern {
    id: "SEC053",
    name: "Hardcoded IV/Nonce",
    description: "Hardcoded initialization vector or nonce weakens encryption",
    severity: "high",
    pattern: r#"(?i)(iv|nonce)\s*[=:]\s*['\"]([a-fA-F0-9]{16,}|[a-zA-Z0-9+/=]{12,})['\"]"#,
    languages: &["all"],
};

// ============================================================================
// Debug Code Patterns
// ============================================================================

/// Console.log with sensitive data
pub const DEBUG_CONSOLE_SENSITIVE: SecurityPattern = SecurityPattern {
    id: "SEC060",
    name: "Debug Console with Sensitive Data",
    description: "Console logging potentially sensitive data",
    severity: "low",
    pattern: r#"(?i)console\.(log|debug|info)\s*\([^)]*(?:password|secret|token|key|credential)"#,
    languages: &["typescript"],
};

/// Print with sensitive data (Python)
pub const DEBUG_PRINT_SENSITIVE: SecurityPattern = SecurityPattern {
    id: "SEC061",
    name: "Debug Print with Sensitive Data",
    description: "Print statement potentially logging sensitive data",
    severity: "low",
    pattern: r#"(?i)print\s*\([^)]*(?:password|secret|token|key|credential)"#,
    languages: &["python"],
};

/// Debug mode enabled
pub const DEBUG_MODE_ENABLED: SecurityPattern = SecurityPattern {
    id: "SEC062",
    name: "Debug Mode Enabled",
    description: "Debug mode should be disabled in production",
    severity: "medium",
    pattern: r#"(?i)(DEBUG\s*[=:]\s*True|debug:\s*true|\.debug\s*=\s*true)"#,
    languages: &["all"],
};

/// Debugger statement
pub const DEBUG_DEBUGGER: SecurityPattern = SecurityPattern {
    id: "SEC063",
    name: "Debugger Statement",
    description: "Debugger statement should be removed in production",
    severity: "low",
    pattern: r#"\bdebugger\b"#,
    languages: &["typescript"],
};

/// Python breakpoint
pub const DEBUG_BREAKPOINT: SecurityPattern = SecurityPattern {
    id: "SEC064",
    name: "Python Breakpoint",
    description: "Breakpoint should be removed in production",
    severity: "low",
    pattern: r#"\b(breakpoint|pdb\.set_trace)\s*\(\s*\)"#,
    languages: &["python"],
};

// ============================================================================
// Additional Security Patterns
// ============================================================================

/// Disabled SSL verification
pub const INSECURE_SSL: SecurityPattern = SecurityPattern {
    id: "SEC070",
    name: "Disabled SSL Verification",
    description: "SSL certificate verification is disabled, enabling MitM attacks",
    severity: "high",
    pattern: r#"(?i)(verify\s*[=:]\s*False|rejectUnauthorized\s*[=:]\s*false|CERT_NONE)"#,
    languages: &["python", "typescript"],
};

/// Hardcoded localhost/IP
pub const HARDCODED_IP: SecurityPattern = SecurityPattern {
    id: "SEC071",
    name: "Hardcoded IP Address",
    description: "Hardcoded IP address or localhost may cause issues in production",
    severity: "low",
    pattern: r#"(?i)['\"](?:localhost|127\.0\.0\.1|0\.0\.0\.0|192\.168\.\d+\.\d+|10\.\d+\.\d+\.\d+)['\"]"#,
    languages: &["all"],
};

/// Unsafe Rust block
pub const UNSAFE_RUST_BLOCK: SecurityPattern = SecurityPattern {
    id: "SEC080",
    name: "Unsafe Rust Block",
    description: "Unsafe Rust code requires careful review",
    severity: "medium",
    pattern: r#"\bunsafe\s*\{"#,
    languages: &["rust"],
};

/// Raw pointer dereference
pub const UNSAFE_RUST_DEREF: SecurityPattern = SecurityPattern {
    id: "SEC081",
    name: "Raw Pointer Dereference",
    description: "Raw pointer dereference requires careful review",
    severity: "medium",
    pattern: r#"\*\s*(mut\s+)?[a-zA-Z_][a-zA-Z0-9_]*\s+as\s+\*"#,
    languages: &["rust"],
};

// ============================================================================
// Pattern Collection
// ============================================================================

/// All available security patterns
pub const ALL_PATTERNS: &[SecurityPattern] = &[
    // Hardcoded secrets
    AWS_ACCESS_KEY,
    AWS_SECRET_KEY,
    GENERIC_API_KEY,
    GENERIC_SECRET,
    PRIVATE_KEY,
    GITHUB_TOKEN,
    SLACK_TOKEN,
    JWT_SECRET,
    // SQL injection
    SQL_INJECTION_CONCAT,
    SQL_INJECTION_FORMAT_PY,
    SQL_INJECTION_TEMPLATE,
    // Command injection
    CMD_INJECTION_SYSTEM,
    CMD_INJECTION_SUBPROCESS,
    CMD_INJECTION_EVAL,
    CMD_INJECTION_EXEC,
    CMD_INJECTION_RUST,
    // Path traversal
    PATH_TRAVERSAL_CONCAT,
    PATH_UNVALIDATED,
    // Insecure deserialization
    INSECURE_PICKLE,
    INSECURE_YAML,
    INSECURE_DESERIALIZE_EVAL,
    // Weak cryptography
    WEAK_CRYPTO_MD5,
    WEAK_CRYPTO_SHA1,
    WEAK_CRYPTO_DES,
    WEAK_CRYPTO_IV,
    // Debug code
    DEBUG_CONSOLE_SENSITIVE,
    DEBUG_PRINT_SENSITIVE,
    DEBUG_MODE_ENABLED,
    DEBUG_DEBUGGER,
    DEBUG_BREAKPOINT,
    // Other security issues
    INSECURE_SSL,
    HARDCODED_IP,
    UNSAFE_RUST_BLOCK,
    UNSAFE_RUST_DEREF,
];

/// Get patterns for a specific language
pub fn patterns_for_language(language: &str) -> Vec<&'static SecurityPattern> {
    ALL_PATTERNS
        .iter()
        .filter(|p| p.languages.contains(&"all") || p.languages.contains(&language))
        .collect()
}

/// Get all critical severity patterns
pub fn critical_patterns() -> Vec<&'static SecurityPattern> {
    ALL_PATTERNS
        .iter()
        .filter(|p| p.severity == "critical")
        .collect()
}

/// Get all high severity patterns
pub fn high_severity_patterns() -> Vec<&'static SecurityPattern> {
    ALL_PATTERNS
        .iter()
        .filter(|p| p.severity == "high" || p.severity == "critical")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn test_all_patterns_compile() {
        for pattern in ALL_PATTERNS {
            let result = Regex::new(pattern.pattern);
            assert!(
                result.is_ok(),
                "Pattern {} failed to compile: {:?}",
                pattern.id,
                result.err()
            );
        }
    }

    #[test]
    fn test_aws_access_key_pattern() {
        let re = Regex::new(AWS_ACCESS_KEY.pattern).unwrap();
        assert!(re.is_match("AKIAIOSFODNN7EXAMPLE"));
        assert!(!re.is_match("not_a_key"));
    }

    #[test]
    fn test_github_token_pattern() {
        let re = Regex::new(GITHUB_TOKEN.pattern).unwrap();
        assert!(re.is_match("ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
        assert!(!re.is_match("not_a_token"));
    }

    #[test]
    fn test_patterns_for_language() {
        let python_patterns = patterns_for_language("python");
        let rust_patterns = patterns_for_language("rust");
        let typescript_patterns = patterns_for_language("typescript");

        // Python should have pickle pattern
        assert!(python_patterns.iter().any(|p| p.id == "SEC040"));
        // Rust should have unsafe block pattern
        assert!(rust_patterns.iter().any(|p| p.id == "SEC080"));
        // TypeScript should have debugger pattern
        assert!(typescript_patterns.iter().any(|p| p.id == "SEC063"));
    }

    #[test]
    fn test_critical_patterns() {
        let critical = critical_patterns();
        assert!(!critical.is_empty());
        for pattern in critical {
            assert_eq!(pattern.severity, "critical");
        }
    }
}
