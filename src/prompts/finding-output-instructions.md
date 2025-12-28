# Structured Finding Output Format

When analyzing code, report findings using this structured format. The qontinui-runner will parse these markers to create a categorized, actionable findings dashboard.

## Basic Format

```
[FINDING:category_id:severity]
Title: The finding title
Description: Detailed description of the finding
File: path/to/file.ts (optional)
Line: 42 (optional)
[/FINDING]
```

## Format with User Input Required

```
[FINDING:category_id:severity:needs_input]
Title: The finding title
Description: Detailed description of the finding
Question: What should we do about this?
Options: Option A | Option B | Option C
File: path/to/file.ts (optional)
Line: 42 (optional)
[/FINDING]
```

## Available Categories

| Category ID | Name | Description | Default Action |
|-------------|------|-------------|----------------|
| `code_bug` | Code Bug | Actual code issues that can be auto-fixed | Auto-fix |
| `todo` | TODO | Tasks that may require user decisions | Needs input |
| `security` | Security | Security vulnerabilities or concerns | Auto-fix |
| `config_issue` | Configuration Issue | Configuration or environment problems | Manual |
| `already_fixed` | Already Fixed | Issues resolved in previous sessions | Informational |
| `expected_behavior` | Expected Behavior | Intentional design, not a bug | Informational |
| `data_migration` | Data Migration | Requires admin or manual intervention | Manual |
| `runtime_issue` | Runtime Issue | Operational issues, not code bugs | Manual |
| `test_issue` | Test Issue | Problems with test code or test setup | Auto-fix |
| `enhancement` | Enhancement | Improvement suggestions | Needs input |
| `documentation` | Documentation | Documentation issues or improvements | Auto-fix |
| `performance` | Performance | Performance issues or optimization opportunities | Needs input |
| `warning` | Warning | Things to be aware of | Informational |

## Severity Levels

| Severity | Use Case |
|----------|----------|
| `critical` | System-breaking issues, security vulnerabilities, data loss risks |
| `high` | Major functionality broken, significant bugs |
| `medium` | Notable issues that should be addressed soon |
| `low` | Minor issues, cosmetic problems |
| `info` | Informational notes, suggestions |

## When to Use `:needs_input`

Add `:needs_input` to the marker when:
- Multiple valid solutions exist and user preference matters
- The fix involves a design decision
- Trade-offs need to be discussed
- You need clarification before proceeding

When using `:needs_input`, always include:
- `Question:` field with a clear question for the user
- `Options:` field with pipe-separated options (if applicable)

## Examples by Category

### Code Bug (Auto-fixable)

```
[FINDING:code_bug:high]
Title: Null pointer exception in user lookup
Description: The getUserById function can return null but the caller doesn't handle this case, causing a crash when looking up deleted users.
File: src/services/UserService.ts
Line: 45
Resolution: Added null check before accessing user properties
[/FINDING]
```

### Security (Critical)

```
[FINDING:security:critical]
Title: SQL injection vulnerability in search endpoint
Description: User input is directly concatenated into SQL query without sanitization. An attacker could inject malicious SQL to access or modify data.
File: backend/routes/search.py
Line: 23
[/FINDING]
```

### TODO (Needs User Input)

```
[FINDING:todo:medium:needs_input]
Title: Implement caching layer for API responses
Description: The API makes redundant calls to the database. Adding caching would improve performance significantly.
Question: Which caching strategy should we use?
Options: Redis (distributed) | In-memory (simple) | Both (hybrid approach)
File: src/api/handlers.ts
Line: 100
[/FINDING]
```

### Enhancement (Needs User Input)

```
[FINDING:enhancement:low:needs_input]
Title: Consider adding dark mode support
Description: Several users have requested dark mode. The UI framework supports theming, so implementation would be straightforward.
Question: Should we add dark mode support?
Options: Yes, implement now | Add to backlog | Not needed
[/FINDING]
```

### Configuration Issue (Manual)

```
[FINDING:config_issue:high]
Title: Missing environment variable for API key
Description: The OPENAI_API_KEY environment variable is not set, causing AI features to fail silently.
File: .env.example
[/FINDING]
```

### Already Fixed (Informational)

```
[FINDING:already_fixed:info]
Title: Login redirect issue
Description: Previously, users were not redirected after login. This was fixed in the last session by updating the auth callback handler.
File: src/auth/callback.ts
Line: 15
Resolution: Added proper redirect logic after successful authentication
[/FINDING]
```

### Expected Behavior (Informational)

```
[FINDING:expected_behavior:info]
Title: Slow startup time on first load
Description: The initial page load takes 2-3 seconds due to lazy loading of heavy components. This is intentional to reduce the main bundle size.
File: src/App.tsx
[/FINDING]
```

### Performance (Needs Input)

```
[FINDING:performance:medium:needs_input]
Title: N+1 query in project list endpoint
Description: Loading 100 projects triggers 100 additional database queries for related data. This could be optimized with eager loading but would increase memory usage.
Question: Should we optimize for speed or memory?
Options: Eager loading (faster, more memory) | Keep current (slower, less memory) | Pagination (compromise)
File: backend/api/projects.py
Line: 67
[/FINDING]
```

### Test Issue (Auto-fixable)

```
[FINDING:test_issue:medium]
Title: Flaky test due to timing issue
Description: The testAsyncOperation test fails intermittently because it doesn't properly await the async operation.
File: tests/async.test.ts
Line: 34
[/FINDING]
```

### Documentation (Auto-fixable)

```
[FINDING:documentation:low]
Title: Outdated API documentation
Description: The README still references the old v1 API endpoints. These were replaced by v2 endpoints three months ago.
File: README.md
Line: 45
[/FINDING]
```

### Warning (Informational)

```
[FINDING:warning:medium]
Title: Deprecated dependency will be removed in next major version
Description: The 'request' package is deprecated and will stop receiving security updates. Consider migrating to 'axios' or 'node-fetch'.
File: package.json
Line: 15
[/FINDING]
```

### Runtime Issue (Manual)

```
[FINDING:runtime_issue:high]
Title: Database connection pool exhausted
Description: The application is running out of database connections during peak load. The current pool size of 10 is insufficient for the traffic volume.
[/FINDING]
```

### Data Migration (Manual)

```
[FINDING:data_migration:high]
Title: User roles need to be migrated to new permission system
Description: The new role-based access control system requires all existing users to have their permissions recalculated based on their current role.
[/FINDING]
```

## Best Practices

1. **Be Specific**: Include file paths and line numbers when available
2. **Be Actionable**: Describe what needs to be done, not just what's wrong
3. **Use Correct Category**: Choose the most appropriate category for accurate dashboard grouping
4. **Set Appropriate Severity**: Be honest about impact - don't over-inflate or under-report
5. **Include Resolution**: If you've already fixed something, include the `Resolution:` field
6. **Ask Good Questions**: When using `:needs_input`, ask clear, specific questions with well-defined options

## Multi-line Content

For longer descriptions, the parser handles multi-line content:

```
[FINDING:code_bug:medium]
Title: Complex validation logic needs refactoring
Description: The validation function in the form handler has grown to over 200 lines
and handles too many responsibilities. It validates input, transforms data,
and makes API calls all in one function.

This violates single responsibility principle and makes testing difficult.
File: src/forms/UserForm.ts
Line: 45
[/FINDING]
```
