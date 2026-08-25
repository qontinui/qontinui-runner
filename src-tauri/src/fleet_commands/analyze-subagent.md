# Analyze with a Subagent (pi or DeepSeek)

Offload a file-analysis task to a subagent and get the analysis back as a tool
result — without spending main-session context reading the files.

Two providers, one tool. **On this fleet both run the same model** — pi's
provider defaults to `deepseek` (`default_pi_cli_provider()` in the runner's
`settings.rs`), so `provider` selects the **execution mode and transport, not
the model**. Pick by the shape of the question, not by which model you want:

| `provider` | How it works | Use it when |
|---|---|---|
| `pi` | Stages **read-only copies** of the files in a temp dir and lets the pi coding agent (running locally, DeepSeek-backed by default) explore them agentically with its own `read` / `grep` / `find` / `ls`. | The answer needs selective or iterative reading — "find concurrency bugs and cite line numbers", or inputs too big to inline. |
| `deepseek` | **One-shot**: inlines the file contents into a single DeepSeek API call via the runner's OpenAI-compatible API client. No agentic exploration. | A straightforward "read these files and answer" question. Cheapest path. **Text files only, 256KB per file, 1MB total.** |

When the input exceeds DeepSeek's inline limits, use `pi` — it reads
selectively rather than inlining.

Because both paths reach DeepSeek, a slow or failing `deepseek` call will not
be rescued by retrying on `pi` (and vice versa) if the cause is the model or
the API account — only the transport and the reading strategy change. They do
fail differently for local reasons: `pi` needs the pi CLI present on the box,
`deepseek` needs the OpenAI-compatible credentials configured.

## Instructions

Call the `analyze_with_subagent` MCP tool (qontinui-wrappers server) with:

- **`provider`** (required): `"pi"` or `"deepseek"` — the enum admits nothing else.
- **`prompt`** (required): the analysis question or instruction. Be specific
  about the output you want (e.g. "list every public function with a one-line
  summary"). For `deepseek` the file contents are inlined *after* your prompt,
  so phrase it as an instruction over the attached files.
- `file_refs` (optional): **absolute** paths of the files to analyze. Optional in
  the schema — a prompt-only call is legal — but it is the point of the tool, so
  supply it unless you deliberately want a bare model call.
- `timeout_secs` (optional): **default 300**. What it controls **differs by
  provider** — this is the one asymmetry between them:
  - `pi` — the real wall-clock budget. `execute_pi` assigns it
    (`settings.timeout_seconds = timeout`) and the pi CLI enforces it. Note this
    **overrides** pi's own configured default of 600s
    (`default_pi_cli_timeout()`), so a subagent pi run is capped at 300s unless
    you pass `timeout_secs` — shorter than pi's normal budget, not longer.
  - `deepseek` — **NOT the API budget.** `execute_deepseek` deliberately
    discards it: the OpenAI-compatible client reads its timeout from
    `ai_settings.openai_compatible.timeout_seconds`, and the runner logs a
    warning when the two differ. Setting `timeout_secs` here does not shorten
    the DeepSeek call.
  Independently of both, the `qontinui-wrappers` bridge waits `timeout_secs + 60`
  on its own HTTP call to the runner. Set it too low on a deepseek call and you
  abort the *bridge*, not the analysis — the runner keeps working and you get a
  dispatch HTTP error.

Usage: `/analyze-subagent <provider> <prompt> <file paths...>` — resolve any
relative paths to absolute against the current working directory before calling
the tool.

Parsing the provider, so the split is decidable: treat the **first** argument as
the provider **if and only if it is exactly `pi` or `deepseek`**. Otherwise the
entire argument string is the prompt and the provider is `pi`. **State which
provider you chose before calling the tool** — a prompt that merely mentions
DeepSeek still runs on pi, and the user should see that.

The tool result is the subagent's analysis text. Weave it into your work; cite
it as "pi subagent analysis" or "DeepSeek subagent analysis" if the user asks
where a conclusion came from.

If the tool is unavailable (qontinui-wrappers not connected), say so and offer
to analyze the files directly instead.

## Provenance

Merged 2026-08-21 from `/analyze-pi` + `/analyze-deepseek`, which were one
command described twice: same MCP tool, same argument list, same fallback,
differing only in the `provider` value — which the tool takes as a required enum
argument, so the split encoded a parameter as a filename.

The contract above is verified against the implementing code, not the tool's
own description string: `subagent_tool_entry` in
`qontinui-runner/src-tauri/src/bin/wrappers_mcp.rs` for the schema, and
`execute_pi` / `execute_deepseek` in `src/subagent/mod.rs` for the per-provider
behaviour. **Verify both layers if you edit this.** The first version of this
merge read only the bridge, saw one provider-agnostic timeout there, and
"corrected" the old deepseek doc's accurate "advisory" note into a claim that
`timeout_secs` binds both providers. It does not — `execute_deepseek` discards
it. Code review caught the inversion before it shipped. The bridge timeout and
the subagent budget are two different clocks, and only the bridge one is
provider-agnostic.

The merge did fix one real error: both old docs presented `file_refs` as
required. The schema's required list is `["provider", "prompt"]` and
`file_refs` carries `#[serde(default)]`, so a prompt-only call is legal.
