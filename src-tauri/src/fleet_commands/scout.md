# Scout — Find & Review Trending GitHub Projects for Qontinui

Discover trending and notable GitHub projects, evaluate their relevance to qontinui, and recommend adoptable ideas, integrations, or techniques.

## Scope & Focus (read before evaluating anything)

**Qontinui is a platform that uses the UI Bridge for intimate access to projects under development.**

The platform automates applications that have the UI Bridge SDK integrated. It is NOT a tool for:

- Automating arbitrary native desktop applications (Windows/Mac/Linux apps that don't ship the SDK)
- Automating third-party websites or external web services
- General-purpose RPA / screen-scraping / browser automation of non-SDK surfaces
- Accessibility-tree-driven automation of apps qontinui doesn't control

**Any external tool — including VLMs, grounding models, computer vision, OCR, accessibility adapters — is justified ONLY if it improves UI Bridge-mediated automation of SDK-instrumented projects.** A project that only applies to non-SDK surfaces is out of scope, regardless of how impressive it is.

**Apply this filter aggressively in Phase 3.** A "computer use agent" that drives arbitrary desktop apps is out of scope. A technique for grounding elements inside an SDK-instrumented React app's canvas/iframe region is in scope. When in doubt, ask: *"does this make the UI Bridge better at driving projects that ship the SDK?"* If no, classify as None/Low regardless of hype.

## The three areas

Qontinui has three distinct areas that benefit from external project scouting:

1. **Runner** — The Tauri 2.5 desktop app (Rust + TypeScript) that orchestrates AI-powered autonomous development workflows, multi-agent pipelines, and task execution against SDK-instrumented targets
2. **UI Bridge** — The SDK for inspecting and interacting with frontend UIs (IPC + HTTP proxy dual-channel). This is the primary surface — everything else exists to make this better
3. **Visual GUI Automation** — The Python core library (OpenCV, state machines, template matching) and multistate library for model-based automation of UI-Bridge-instrumented projects. Visual techniques are fallbacks for gaps inside SDK apps (canvas/iframe contents, un-wrapped elements, NL disambiguation), not tools for non-SDK automation

## Instructions

### Phase 1: Gather Candidate Projects

Use multiple discovery strategies to build a candidate list. Try all available approaches:

1. **GitHub Trending Page** — Fetch `https://github.com/trending` and `https://github.com/trending?since=weekly` using WebFetch. Parse the page for project names, descriptions, and star counts. Also check language-specific trending pages relevant to qontinui:
   - `https://github.com/trending/python?since=weekly`
   - `https://github.com/trending/rust?since=weekly`
   - `https://github.com/trending/typescript?since=weekly`

2. **GitHub Search API** — Search for recently-created or recently-updated repos in relevant domains using the `gh` CLI. Organize searches by category:

   **Runner-related searches:**
   ```bash
   # AI-powered development, coding agents
   gh search repos "ai coding agent" --sort=stars --limit=15 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "autonomous developer" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "agentic workflow" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "workflow orchestration" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language

   # Tauri ecosystem
   gh search repos "tauri plugin" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "tauri desktop" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language

   # Multi-agent systems
   gh search repos "multi agent pipeline" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "agent orchestration" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language

   # Meta-optimization, learning systems
   gh search repos "prompt optimization" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "llm evaluation" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   ```

   **UI Bridge-related searches:**
   ```bash
   # UI testing, inspection, accessibility
   gh search repos "ui testing sdk" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "accessibility tree" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "dom snapshot" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "ui inspection" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "visual regression testing" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "component testing" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   ```

   **Visual GUI Automation-related searches:**
   ```bash
   # GUI automation, RPA, visual testing
   gh search repos "gui automation" --sort=stars --limit=15 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "visual automation" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "RPA" --sort=updated --limit=10 --json fullName,description,stargazersCount,updatedAt,language

   # Computer vision for UI
   gh search repos "computer vision UI" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "screen recognition" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "computer use agent" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language

   # State machines
   gh search repos "state machine" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   gh search repos "template matching" --sort=stars --limit=10 --json fullName,description,stargazersCount,updatedAt,language
   ```

3. **User-Suggested URLs** — If the user provides specific GitHub URLs or topics, include those.

4. **$ARGUMENTS** — If arguments are provided, treat them as additional search terms or GitHub URLs to include.

**Parallelism:** Launch multiple search agents in parallel to speed up discovery. Use subagents for independent search queries.

---

### Phase 2: Filter Already-Analyzed Projects

Read ALL THREE tracking files to build a combined set of already-analyzed projects:
- `qontinui-dev-notes/github-scout/runner.md`
- `qontinui-dev-notes/github-scout/ui-bridge.md`
- `qontinui-dev-notes/github-scout/visual-gui-automation.md`

Extract all previously reviewed project names (in `owner/name` format). Remove any candidates that already appear in any tracking file.

If a project was reviewed more than 90 days ago, it MAY be re-reviewed if it has had significant updates since then. Note this as a "re-review" in the output.

---

### Phase 3: Evaluate Candidates

For each new candidate (up to 15 most promising), fetch the project's README and evaluate relevance to qontinui. Use WebFetch to read `https://github.com/{owner}/{name}` or use `gh repo view {owner}/{name}` for details.

**Qontinui context for evaluation:**
- **Runner**: Tauri 2.5 desktop app (Rust + TypeScript), AI-powered autonomous development, multi-agent pipeline orchestration, verification-agentic loops, meta-optimizer learning system, MCP integration
- **UI Bridge**: SDK for UI inspection/interaction, IPC + HTTP proxy dual-channel, DOM snapshots, element targeting, frontend verification assertions
- **Visual GUI Automation**: Python core library with OpenCV template matching, multistate state machines (multi-active-state model with pathfinding), AI-assisted element detection (UI-TARS, VisionLLM fallback), HAL abstraction layer

**Evaluate each project on:**
1. **SDK-scope fit (gate)** — Does this serve UI-Bridge-mediated automation of SDK-instrumented projects? If the project only works on non-SDK surfaces (native apps qontinui doesn't control, arbitrary websites, generic RPA), classify relevance as None or Low regardless of other merits. Do not recommend adoption for work qontinui is explicitly not doing.
2. **Direct applicability** — Could this be integrated into or used by qontinui to drive SDK-instrumented projects?
3. **Technique transfer** — Does it use techniques qontinui could adopt inside its SDK-scoped automation?
4. **Competitive insight** — Is it a competitor or alternative that reveals gaps in qontinui's SDK-instrumented offering?
5. **Ecosystem value** — Does it enhance the broader ecosystem qontinui operates in (agents, LLM orchestration, Tauri tooling, dev-UX)?
6. **Community/traction** — Stars, recent activity, maintenance quality

**Classify each project into one of the three categories:**
- **Runner** — Relates to workflow orchestration, AI agents, Tauri, multi-agent systems, task execution
- **UI Bridge** — Relates to UI testing/inspection, accessibility, DOM manipulation, visual verification
- **Visual GUI Automation** — Relates to computer vision, template matching, state machines, screen recognition, RPA

Some projects may span categories. Choose the primary category and note secondary relevance.

**Relevance levels:**
- **High** — Directly applicable to UI-Bridge-mediated SDK-instrumented automation (integration candidate, technique qontinui would adopt inside its SDK scope, or a direct competitor in that same scope)
- **Medium** — Interesting approach that could inform qontinui's SDK-scoped roadmap (e.g., agent orchestration patterns, dev-UX improvements for SDK-instrumented apps)
- **Low** — Tangentially related or applies mostly to non-SDK surfaces — worth watching only
- **None** — Not relevant to qontinui's SDK-scoped mission, OR solely targets non-SDK automation (even if technically impressive)

---

### Phase 4: Report & Record

#### Report to User

Present findings in a structured format, grouped by category:

```
## Scout Report — YYYY-MM-DD

### Runner
#### High Relevance
1. **owner/name** (★ N, Language) — Summary
   - Why it matters: ...
   - Takeaways: ...
   - Suggested action: ...

#### Medium/Low Relevance
...

### UI Bridge
#### High Relevance
...

### Visual GUI Automation
#### High Relevance
...

### Already Analyzed (skipped)
- owner/name (reviewed YYYY-MM-DD, category)
...
```

Focus the report on **actionable recommendations**. For high-relevance projects, be specific about what qontinui could adopt and where in the codebase it would apply.

#### Record in Tracking Files

Append each analyzed project to the appropriate tracking file based on its primary category:
- Runner projects → `qontinui-dev-notes/github-scout/runner.md`
- UI Bridge projects → `qontinui-dev-notes/github-scout/ui-bridge.md`
- Visual GUI Automation projects → `qontinui-dev-notes/github-scout/visual-gui-automation.md`

Use this format for each entry:

```markdown
## [owner/name](https://github.com/owner/name)
- **Reviewed:** YYYY-MM-DD
- **Stars:** N | **Language:** X
- **Summary:** One-line description
- **Qontinui Relevance:** High / Medium / Low / None
- **Takeaways:** What qontinui could learn, adopt, or integrate
- **Action:** None / Watch / Integrate idea / Open issue / etc.
```

---

## Rules

- **Do NOT modify any project code.** This is a research-only command.
- **Be skeptical of hype.** A trending repo with 50 stars and no tests is not necessarily worth adopting.
- **Prioritize quality over quantity.** It's better to deeply evaluate 5 high-relevance projects than to skim 30.
- **Always record results** in the appropriate tracking file so projects aren't re-analyzed unnecessarily.
- **Use parallel agents** for fetching and evaluating multiple projects simultaneously.
- **If WebFetch fails** for trending pages, fall back to `gh` CLI searches and ask the user if they can provide URLs.
