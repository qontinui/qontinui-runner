/**
 * Security & Sandboxing Tutorial
 *
 * Informational tutorial explaining the runner's layered agent security system —
 * what it protects against, how the 7-layer AST model works, and what each
 * UI control does. Covers profiles, container hardening, network mediation,
 * credential proxy, policy engine, and audit trail.
 */

import type { Tutorial } from "../../../types/tutorial";

export const securitySandboxingTutorial: Tutorial = {
  id: "security-sandboxing",
  title: "Security & Sandboxing for AI Agents",
  description:
    "Understand the runner's layered security system — how it isolates agent execution with container hardening, network mediation, credential proxy, and policy-driven action governance following the AST 7-layer defense model.",
  duration: "10 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "settings",
  category: "Architecture",
  tags: [
    "security",
    "sandboxing",
    "containers",
    "featured",
    "architecture",
    "ast",
  ],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand why agent sandboxing matters for autonomous workflows",
    "Know the AST 7-layer defense model and what each layer protects",
    "Understand the three built-in profiles and when to use each",
    "Know how the credential proxy prevents API key exposure",
    "Understand how the network mediation proxy filters traffic",
    "Know how to read and use the audit trail",
  ],
  steps: [
    {
      id: "intro",
      title: "Why Agent Security Matters",
      content: `When the runner executes workflows autonomously, steps can run **shell commands**, **make API calls**, **access the filesystem**, and **interact with UIs**. Without security boundaries, a misconfigured workflow or a prompt injection could:

- **Exfiltrate API keys** by reading environment variables and sending them to an external server
- **Delete files** with destructive commands like \`rm -rf /\`
- **Access cloud metadata** endpoints to steal infrastructure credentials
- **Make unauthorized network requests** to external services

The security system creates **layered defenses** so that even if one layer is bypassed, others still protect the system.`,
      estimatedDuration: 1,
      tips: [
        "Security is opt-in — the default 'permissive' profile reproduces current behavior exactly",
        "You can upgrade to 'standard' or 'strict' profiles at any time without changing workflows",
      ],
    },
    {
      id: "ast-model",
      title: "The AST 7-Layer Defense Model",
      content: `The security system follows the **Agent Sandbox Taxonomy (AST)** — an open framework for evaluating agent sandboxes across 7 defense layers:

| Layer | Name | What it protects |
|-------|------|-----------------|
| **L1** | Compute | Process isolation (containers, capabilities) |
| **L2** | Resources | CPU, memory, PID limits |
| **L3** | Filesystem | Which paths can be read/written |
| **L4** | Network | Which domains can be accessed |
| **L5** | Credentials | How API keys are handled |
| **L6** | Actions | Which commands are allowed |
| **L7** | Audit | Logging of security decisions |

Each layer has a **score from 0-3** indicating enforcement level: None (0), Cooperative (1), Software-enforced (2), Kernel-enforced (3).

The **Security Posture** visualization in Settings shows these scores as colored bars.`,
      estimatedDuration: 1,
      details: `The AST model comes from the open-source Agent Sandbox Taxonomy project, which evaluated 26 different agent sandbox products. Its key insight: "sandboxed" is not binary — most products score well on compute isolation (L1) but fail on network (L4) and credentials (L5).

The fingerprint notation \`L1:3/L2:3/L3:3/L4:3/L5:3/L6:3/L7:3\` gives a compact summary of the security posture. The strict profile achieves 3/3/3/3/3/3/3 (all layers kernel/software-enforced).`,
    },
    {
      id: "profiles",
      title: "Security Profiles — Permissive, Standard, Strict",
      content: `Three built-in profiles provide pre-configured security postures:

**Permissive** (default) — No restrictions. Current behavior. Suitable for trusted workflows you've written yourself.

**Standard** (recommended) — Balanced security:
- Capabilities dropped, non-root user in containers
- Destructive commands blocked (\`rm -rf\`, \`DROP TABLE\`, \`git push --force\`)
- \`sudo\` and \`su\` blocked
- Network in allowlist mode (only configured domains)
- Credential proxy enabled (agents never see real API keys)
- PID limit: 256, memory: 2GB

**Strict** — Maximum isolation for untrusted code:
- All capabilities dropped, seccomp syscall filtering
- Read-only root filesystem
- Network disabled by default
- Only \`/workspace/output\` is writable
- PID limit: 64, timeout: 120s

You can also create a **Custom** profile with per-sub-policy configuration.`,
      estimatedDuration: 1,
      tips: [
        "The profile selector is in Settings > Security",
        "Per-workflow overrides let different workflows run at different security levels",
        "The 'security_profile' field on a workflow overrides the global default",
      ],
    },
    {
      id: "container-hardening",
      title: "L1/L2 — Container Hardening",
      content: `When Docker is available, workflow steps execute inside **ephemeral containers** with security hardening applied by the active profile:

**Capability dropping** — Linux capabilities control what a process can do (mount filesystems, trace processes, change network config). The \`standard\` and \`strict\` profiles drop ALL capabilities (\`cap_drop: ["ALL"]\`) and only add back what's needed.

**Seccomp filtering** — A compiled syscall filter blocks dangerous operations at the kernel level: \`mount\`, \`ptrace\`, \`reboot\`, \`sethostname\`, \`bpf\`, and 50+ others. Even if an agent escapes the container process, it can't call these syscalls.

**Non-root execution** — Containers run as user \`1000:1000\`, not root.

**Resource limits** — CPU cores, memory MB, PID count, and execution timeout prevent resource exhaustion.

**Read-only rootfs** — The container filesystem is read-only with tmpfs mounts for \`/tmp\` and \`/var/tmp\`.`,
      estimatedDuration: 1,
      details: `The seccomp profile is embedded at compile time as \`seccomp_default.json\`. It uses the \`SCMP_ACT_ALLOW\` default action with a deny list of dangerous syscalls — this is the standard "deny dangerous, allow rest" approach used by Docker's default profile, but stricter.

The \`policy_to_container_config()\` function translates the abstract \`SecurityPolicy\` into concrete Docker \`ContainerConfig\` settings (cap_drop, seccomp, user, pids_limit, etc.). This happens per-execution via \`execute_with_policy()\`.`,
    },
    {
      id: "network-mediation",
      title: "L4 — Network Mediation Proxy",
      content: `Instead of binary "network on/off", the security system runs a **domain-filtering forward proxy** on the host:

1. Container traffic is routed through the proxy via \`HTTP_PROXY\`/\`HTTPS_PROXY\` env vars
2. For HTTPS (CONNECT tunneling): the proxy reads the target domain and evaluates it against the policy
3. **Allowed domains** pass through — the proxy creates a TCP tunnel
4. **Denied domains** get a \`403 Forbidden\` response
5. **Cloud metadata endpoints** (169.254.169.254, fd00:ec2::254) are always blocked

The proxy supports three modes:
- **AllowList** — only listed domains can be accessed (standard profile)
- **DenyList** — all domains except listed ones are allowed
- **Disabled** — all network access blocked (strict profile default)

Each request is logged to the audit trail with the domain, protocol, and decision.`,
      estimatedDuration: 1,
      tips: [
        "The standard profile pre-allows api.anthropic.com, api.openai.com, and generativelanguage.googleapis.com",
        "Wildcard patterns like *.openai.com match any subdomain",
        "The proxy does NOT perform TLS MITM — it filters at the domain level only",
      ],
    },
    {
      id: "credential-proxy",
      title: "L5 — Credential Proxy (Placeholder Injection)",
      content: `The credential proxy ensures that **agent processes never see real API keys**. Here's how it works:

1. Before container creation, the system generates **random placeholder tokens** for each credential (e.g., \`ANTHROPIC_API_KEY=QCRED_CLAUDE_API_a8f3e2b1c9d0f4e7\`)
2. These placeholders are injected as env vars into the container
3. When the agent makes an API call, the placeholder appears in the \`Authorization\` or \`x-api-key\` header
4. The network mediation proxy **intercepts the request** and **replaces the placeholder** with the real credential from the OS keychain
5. The real key is injected into the outbound request — the agent never had it

This is inspired by the **gondolin** project's secret placeholder injection pattern. Even if the agent runs \`env\` or reads \`/proc/self/environ\`, it only sees placeholder tokens.`,
      estimatedDuration: 1,
      details: `The \`CredentialProxy\` struct generates placeholder tokens using 64 bits of randomness per credential. It maps known credential names to their keychain sources:

- \`claude_api\` → \`com.qontinui.runner.ai / claude_api_key\`
- \`openai\` → \`com.qontinui.runner.ai / openai_api_key\`
- \`gemini\` → \`com.qontinui.runner.gemini / gemini_api_key\`

The proxy only injects credentials into HTTPS requests — plain HTTP credential injection is refused to prevent plaintext transmission.`,
    },
    {
      id: "action-governance",
      title: "L6 — Action Governance",
      content: `The policy engine evaluates every command before execution:

**Destructive command detection** — 16 compiled regex patterns catch dangerous operations:
- \`rm -rf\`, \`mkfs\`, \`dd of=/dev/\`
- \`DROP TABLE\`, \`TRUNCATE TABLE\`
- \`git push --force\`, \`git reset --hard\`
- \`shutdown\`, \`reboot\`, \`killall\`

**Blocked command patterns** — Custom regex patterns (configured per-profile or in the custom editor) block specific commands. Standard profile blocks \`sudo\` and \`su\`.

**Command allowlisting** — Optionally restrict execution to only named commands.

**Filesystem policy** — Working directories are checked against allowed/denied path patterns using glob matching (\`**\` for multi-segment, \`*\` for single-segment).

All blocked commands are logged to the audit trail with the denial reason.`,
      estimatedDuration: 1,
      tips: [
        "Regex patterns are compiled once and cached for performance (LazyLock + HashMap cache)",
        "Glob patterns like /proc/*/environ and /home/*/.ssh/** work correctly",
        "Filesystem enforcement checks write access to the working directory before shell command execution",
      ],
    },
    {
      id: "audit-trail",
      title: "L7 — Security Audit Trail",
      content: `Every security decision is logged to the **audit trail** — a PostgreSQL-backed event log that records:

- **Policy evaluations** — which profile was applied, what was allowed/denied
- **Network requests** — domain, protocol, decision (allowed/denied/warning)
- **Credential injections** — which credential was injected for which domain (without the actual value)
- **Command blocks** — which commands were denied and why
- **Container lifecycle** — container creation, start, stop, timeout events

The audit trail uses an **async batch writer** — events are sent via an mpsc channel to a background task that flushes them to PostgreSQL every 2 seconds or when 50 events accumulate. This avoids blocking the execution pipeline.

You can view the audit trail in **Settings > Security** when audit logging is enabled.`,
      estimatedDuration: 1,
      details: `The audit log viewer in the UI shows:
- **Summary badges** — counts of allowed, denied, and warning events
- **Filter dropdown** — filter by decision type (all/denied/allowed/warnings)
- **Event table** — timestamp, type, decision (color-coded badge), action, reason
- **Export** — download events as JSON for external analysis

The MCP API exposes three endpoints:
- \`GET /security/audit/events?limit=50&decision=denied\`
- \`GET /security/audit/summary\`
- \`DELETE /security/audit/cleanup?older_than_days=30\`

Old events are automatically cleaned up on startup based on the configured retention period.`,
    },
    {
      id: "policy-engine",
      title: "How Policies Are Resolved",
      content: `The **PolicyEngine** resolves the effective security policy for each step using a priority chain:

1. **Per-workflow override** — the \`security_profile\` field on a workflow config
2. **Settings default** — the profile selected in Settings > Security
3. **Built-in permissive** — absolute fallback (no restrictions)

The resolved policy is injected into the \`HandlerContext\` that every step handler receives. This means security is enforced at the handler boundary — it doesn't matter whether the step is a shell command, AI prompt, UI Bridge action, or nested workflow.

**Enforcement points:**
- \`ShellCommandHandler\` — evaluates command + filesystem policy
- \`PromptStepHandler\` — logs AI session audit events
- \`UiBridgeHandler\` — evaluates URL against network policy
- \`IsolatedExecutor\` — applies container hardening via \`policy_to_container_config()\`
- \`NetworkMediator\` — filters outbound traffic
- \`CredentialProxy\` — injects placeholder tokens`,
      estimatedDuration: 1,
    },
    {
      id: "custom-editor",
      title: "Custom Profile Editor",
      content: `When you select the **Custom** profile, a full sub-policy editor appears:

**Network** — Choose a mode (Unrestricted, Allow List, Deny List, Disabled), add domains, toggle metadata endpoint blocking.

**Actions** — Toggle destructive command blocking, add custom blocked command regex patterns.

**Filesystem** — Toggle read-only root filesystem, add denied path glob patterns.

**Resources** — Set CPU limit (cores), memory (MB), PID limit, and timeout (seconds).

**Credentials** — Choose mode: Direct (current behavior), Proxy (placeholder injection), or Denied (no credentials).

The custom policy is saved as part of your security settings and persists across sessions. You can switch between built-in profiles and your custom profile without losing the custom configuration.`,
      estimatedDuration: 1,
      tips: [
        "The Security Posture bars update in real-time as you change profile settings",
        "Custom profile defaults to a moderate baseline — start there and adjust",
        "Domain patterns support wildcards: *.example.com matches any subdomain",
      ],
    },
    {
      id: "summary",
      title: "Putting It All Together",
      content: `The security system provides **defense in depth** — no single layer is the complete solution, but together they cover the full threat surface:

| Threat | Protection |
|--------|-----------|
| Code execution escape | L1: Container isolation, dropped capabilities, seccomp |
| Resource exhaustion | L2: CPU, memory, PID limits, timeouts |
| File theft/corruption | L3: Read-only rootfs, denied path patterns |
| Data exfiltration | L4: Network allowlist, metadata endpoint blocking |
| API key theft | L5: Credential proxy (agent never sees real keys) |
| Destructive operations | L6: Command blocklist, destructive detection |
| Incident investigation | L7: Full audit trail with query API |

**Getting started:**
1. Go to **Settings > Security**
2. Select the **Standard** profile
3. Enable **Security Audit Log** to see decisions in real-time
4. Run a workflow — check the audit viewer for events
5. Adjust domain allowlists and command patterns as needed`,
      estimatedDuration: 1,
      resources: [
        {
          title: "Agent Sandbox Taxonomy (AST)",
          url: "https://github.com/kajogo777/the-agent-sandbox-taxonomy",
        },
        {
          title: "nono — Kernel-enforced agent sandbox",
          url: "https://github.com/always-further/nono",
        },
      ],
    },
  ],
};
