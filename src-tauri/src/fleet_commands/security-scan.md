# Security Vulnerability Scan

Perform a comprehensive security audit of the codebase.

## Instructions

**IMPORTANT**: This command is primarily READ-ONLY for analysis. It may offer to fix critical issues with user approval.

---

### Phase 1: Automated Security Scanning

#### Python Projects

```bash
cd $PWD/qontinui-devtools

# Run security scan
poetry run qontinui-devtools security scan /path/to/project
```

Also run:
```bash
cd /path/to/project

# Check for known vulnerabilities in dependencies
pip-audit  # if available
poetry run safety check  # if safety installed

# Static analysis for security
poetry run bandit -r . -f json 2>/dev/null || poetry run bandit -r .
```

#### JavaScript/TypeScript Projects

```bash
cd /path/to/project

# Check for vulnerable dependencies
npm audit
npm audit --json  # For detailed output

# Check for secrets in code
npx secretlint "**/*"  # if available
```

---

### Phase 2: Manual Security Review

Search for common vulnerability patterns:

#### Hardcoded Secrets
```bash
# Search for potential secrets
grep -rn "password\s*=\s*['\"]" /path/to/project --include="*.py"
grep -rn "api_key\s*=\s*['\"]" /path/to/project --include="*.py"
grep -rn "secret\s*=\s*['\"]" /path/to/project --include="*.py"
grep -rn "token\s*=\s*['\"]" /path/to/project --include="*.py"
grep -rn "AWS_\|AZURE_\|GCP_" /path/to/project --include="*.py"
```

#### SQL Injection
```python
# VULNERABLE:
query = f"SELECT * FROM users WHERE id = {user_id}"
cursor.execute(query)

# SAFE:
cursor.execute("SELECT * FROM users WHERE id = %s", (user_id,))
```

#### Command Injection
```python
# VULNERABLE:
os.system(f"ls {user_input}")
subprocess.run(f"echo {user_input}", shell=True)

# SAFE:
subprocess.run(["ls", user_input], shell=False)
```

#### Path Traversal
```python
# VULNERABLE:
file_path = f"/uploads/{user_filename}"
open(file_path)

# SAFE:
safe_path = pathlib.Path("/uploads") / user_filename
if safe_path.resolve().is_relative_to("/uploads"):
    open(safe_path)
```

#### XSS (for web applications)
```python
# VULNERABLE:
return f"<div>{user_input}</div>"

# SAFE:
from markupsafe import escape
return f"<div>{escape(user_input)}</div>"
```

#### Insecure Deserialization
```python
# VULNERABLE:
import pickle
data = pickle.loads(user_data)

# SAFE:
import json
data = json.loads(user_data)
```

---

### Phase 3: Authentication & Authorization Review

Check for:

1. **Password handling**:
   - Passwords hashed with strong algorithms (bcrypt, argon2)
   - No plaintext password storage
   - Secure password reset flows

2. **Session management**:
   - Secure session tokens
   - Proper session expiration
   - CSRF protection

3. **Authorization**:
   - Proper access control checks
   - No privilege escalation paths
   - Consistent authorization across endpoints

---

### Phase 4: Configuration Security

Check for:

1. **Debug mode**: Ensure DEBUG=False in production configs
2. **CORS**: Restrictive CORS policies
3. **HTTPS**: TLS/SSL enforcement
4. **Headers**: Security headers (CSP, X-Frame-Options, etc.)
5. **Cookies**: Secure, HttpOnly, SameSite flags

---

### Phase 5: Dependency Vulnerabilities

```bash
# Python
pip-audit
poetry show --tree | grep -i "security\|vulnerable"

# JavaScript
npm audit --audit-level=moderate
```

Create a table of vulnerable dependencies:

| Package | Version | Vulnerability | Severity | Fix Version |
|---------|---------|---------------|----------|-------------|

---

### Phase 6: Generate Security Report

````markdown
# Security Scan Report: {project}
Date: {date}

## Executive Summary
- Critical vulnerabilities: {count}
- High severity: {count}
- Medium severity: {count}
- Low severity: {count}

## Critical Findings (Immediate Action Required)

### SEC-001: {Title}
- **Severity**: Critical
- **Category**: {e.g., SQL Injection, Hardcoded Secret}
- **Location**: `path/to/file.py:123`
- **Description**: {What the vulnerability is}
- **Impact**: {What could happen if exploited}
- **Remediation**: {How to fix}
- **Code Example**:
  ```python
  # Before (vulnerable):
  ...
  # After (fixed):
  ...
  ```

## High Severity Findings
...

## Medium Severity Findings
...

## Low Severity Findings
...

## Dependency Vulnerabilities

| Package | Installed | Vulnerability | Severity | Action |
|---------|-----------|---------------|----------|--------|

## Security Checklist

- [ ] No hardcoded secrets
- [ ] SQL queries parameterized
- [ ] User input validated/sanitized
- [ ] Authentication properly implemented
- [ ] Authorization checks in place
- [ ] Dependencies up to date
- [ ] Security headers configured
- [ ] HTTPS enforced
- [ ] Logging doesn't expose sensitive data
- [ ] Error messages don't leak info

## Recommendations

### Immediate (This Sprint)
1. {critical fix}

### Short-term (This Month)
1. {high priority fix}

### Long-term (Backlog)
1. {improvements}
````

---

### Phase 7: Optional - Fix Critical Issues

If critical vulnerabilities are found:

1. **Ask for user approval** before making changes
2. **Fix only critical security issues**
3. **Create a separate commit** for security fixes
4. **Run tests** to verify fixes don't break functionality

---

### Output

Present the security report directly in the chat.

Flag any critical issues prominently for immediate attention.
