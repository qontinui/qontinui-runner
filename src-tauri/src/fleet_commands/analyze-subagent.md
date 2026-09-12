# Analyze with a Subagent (pi = agentic, deepseek = one-shot)

Offload a file-analysis task to a subagent and get the analysis back as a tool
result — without spending main-session context reading the files.

Two providers, one tool: `provider` selects the **execution mode and
transport**, never the model. Pick by the shape of the question, not by which
model you want:

| `provider` | How it works | Use it when |
|---|---|---|
| `pi` | Stages **read-only copies** of the files in a temp dir and lets the pi coding agent (running locally, DeepSeek-backed by default) explore them agentically with its own `read` / `grep` / `find` / `ls`. | The answer needs selective or iterative reading — "find concurrency bugs and cite line numbers", or inputs too big to inline. |
| `deepseek` | **One-shot**: inlines the file contents into a single call to the configured OpenAI-compatible endpoint (DeepSeek by default). No agentic exploration. | A straightforward "read these files and answer" question. Cheapest path. **Text files only, 256KB per file, 1MB total.** |

When the input exceeds those inline limits, use `pi` — it reads selectively
rather than inlining. The limits are the **runner's** (`MAX_FILE_BYTES` /
`MAX_TOTAL_BYTES`), enforced before any API call, not the upstream model's
context window.

Neither provider name pins a model, and **the two need not be running the same
one.** `deepseek` dispatches through `run_prompt_with_model_override` with
`provider_override = "openai_compatible"` and the model read from
`ai_settings.openai_compatible.model` — default `deepseek-chat` against
`https://api.deepseek.com`. `pi`'s `PiCliSettings::model` defaults to `None`,
and `build_flag_args` emits `--model` only when it is set, so pi runs *its own*
default model for the `deepseek` provider — whatever that is on the installed
pi version. Repoint either setting and the pair diverges silently. Treat
`provider` as the choice of execution mode; if you need a specific model, pin
it in settings rather than inferring it from the provider name.

**Usually the same account, but that is a fallback rather than a coupling, and
the precedence is inverted.** pi's provider defaults to `deepseek`
(`default_pi_cli_provider()`), and `deepseek_key_env` then feeds it the
`openai_compatible` keychain key — but only if `DEEPSEEK_API_KEY` is *not*
already in the runner's environment, which pi inherits. The other path resolves
keychain **first** and falls back to that same env var second
(`resolve_api_key` in `ai_provider/openai_compat.rs`). Nothing holds the two on
one account: with a keychain entry and a *different* `DEEPSEEK_API_KEY` in the
environment they split, and the runner cannot tell that they have — when
`deepseek_key_env` declines, it injects nothing and pi resolves its own key from
the inherited environment or `~/.pi/agent/settings.json`, which the runner never
reads. Nor is the upstream guaranteed shared: `openai_compatible.base_url` is
explicitly repointable (the settings comment names keyless local endpoints), and
repointing it leaves `deepseek` not talking to DeepSeek at all while `pi` still
points wherever its own `deepseek` provider does.

Where they *do* land on the same account and endpoint — the common case — a
slow or failing `deepseek` call will not be rescued by retrying on `pi`, or the
reverse: only the transport and the reading strategy change. They fail
differently for local reasons (`pi` needs the pi CLI on the box, `deepseek`
needs the OpenAI-compatible credentials configured), and a *model*-specific
failure is the one upstream case where switching can genuinely help, precisely
because the two need not be on the same model.

## Instructions

Call the `analyze_with_subagent` MCP tool (qontinui-wrappers server) with:

- **`provider`** (required): `"pi"` or `"deepseek"` — the enum admits nothing else.
- **`prompt`** (required): the analysis question or instruction. Be specific
  about the output you want (e.g. "list every public function with a one-line
  summary"). For `deepseek` the file contents are inlined *after* your prompt,
  so phrase it as an instruction over the attached files.
- `file_refs` (optional): **absolute** paths of the files to analyze. Optional in
  the schema — a prompt-only call is legal — but it is the point of the tool, so
  supply it unless you deliberately want a bare model call. Every ref is
  validated before *either* provider runs, and one bad ref fails the whole call
  — see "What the subagent actually sees" below.
- `timeout_secs` (optional): **default 300**. What it controls **differs by
  provider** — this is the one asymmetry between them:
  - `pi` — the real wall-clock budget, but **the runner enforces it, not pi.**
    `execute_pi` sets `settings.timeout_seconds = timeout` on an **in-memory
    clone** of `pi_cli` — nothing is written back to disk — `run_pi_cli_in_dir`
    turns that into a `Duration`, and `execute_argv` holds the deadline. What
    it guarantees is that `execute_argv` *returns* on time; the kill behind it
    is weaker than it looks. On Windows it is `taskkill /F /T`, falling back to
    a plain kill if that fails; off Windows it kills only the direct child. The
    code's own note is that a surviving grandchild can still hold the pipes, so
    read the tree kill as best-effort, not a guarantee.

    **The runner passes pi no timeout at all.** `build_flag_args` emits `-p`,
    `--no-session`, `--provider`, `--model` and `--tools` and nothing else, and
    the only environment it adds is `QONTINUI_TRACE_ID` plus the optional
    `DEEPSEEK_API_KEY`. Whether pi runs some deadline of its own from
    `~/.pi/agent/settings.json` is not visible from this repo — do not assume
    either way; what is certain is that nothing here sets one.

    The assignment **replaces** the configured value — it does not necessarily
    shorten it. 600s is only the value on a default install
    (`default_pi_cli_timeout()` fires only when the key, or the whole `pi_cli`
    block, is absent), and the field is writable through `PUT /settings/ai` or
    by editing the settings file — the Tauri `save_ai_settings` commands carry
    the existing `pi_cli` block through untouched, so there is no settings-UI
    route to it. Against a default install a subagent run caps pi at 300s;
    against a persisted 120s it *raises* the budget. Only the 300s default is
    fixed, and only while you pass no `timeout_secs`.
  - `deepseek` — **NOT the API budget.** `execute_deepseek` deliberately
    discards it: the OpenAI-compatible client reads its timeout from
    `ai_settings.openai_compatible.timeout_seconds`, and the runner logs a
    warning when the two differ. Setting `timeout_secs` here does not shorten
    the DeepSeek call.
  Independently of both, the `qontinui-wrappers` bridge waits `timeout_secs + 60`
  on its own HTTP call to the runner. Set it too low on a deepseek call and you
  abort the *bridge*, not the analysis — the runner keeps working and you get a
  dispatch HTTP error.

  **On defaults the bridge is already the shorter clock for `deepseek`, and no
  setting of `timeout_secs` closes the gap.** Omitting it gives the bridge
  300 + 60 = **360s**, while `default_openai_compatible_timeout()` is **600s** —
  and that 600s is a *per-attempt* HTTP timeout, not the path's budget.
  `routing.rs` wraps the call in `retry_with_backoff_tracked`, and a reqwest
  timeout matches `is_retryable_error`, so it is retried `MAX_AI_RETRIES = 3`
  more times with 2/4/8s backoff — **up to ~40 minutes**, with the rate-limit
  arm able to reset the attempt counter on top of that. The circuit breaker does
  not shorten that: `is_provider_available` is checked once *before* the loop, so
  it can refuse a call outright but is never re-consulted between attempts. The
  bridge can only ever bound the *first* attempt. Raising `timeout_secs` buys headroom
  (`540` gets the bridge to 600s) but does not make the two clocks agree; when
  a deepseek call is genuinely slow, expect a dispatch HTTP error while the
  runner keeps working, and note that the finished text is then discarded
  rather than returned — `execute_analysis` runs inside `spawn_blocking`, which
  the client disconnect does not cancel, and nothing persists the result. The
  tokens are spent either way.

  **Never pass `timeout_secs: 0`.** It does not mean "no limit", and it inverts
  the clocks on the `pi` path too: `unwrap_or` replaces only a *missing* value,
  so the zero survives to `execute_pi`, `run_pi_cli_in_dir` falls back to
  **600s** for its own deadline when `timeout_seconds == 0`, and the bridge
  meanwhile waits 0 + 60 = **60s**. Same discarded-result outcome, one minute
  in. That 600 is a hardcoded literal in `run_pi_cli_in_dir`. It is *not*
  "pi's fallback" — the runner passes pi nothing — and it is *not*
  `default_pi_cli_timeout()`, which is a separate 600 in `settings.rs`. The two
  are equal today and nothing keeps them so.

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

## What the subagent actually sees

`validate_file_refs` runs first, for both providers, and returns on the first
bad ref — so a single mistake in the list costs the whole call, before any
model is billed. Every path must be **absolute** and must resolve to an
**existing regular file**. A directory is rejected: there is no recursive
expansion, so enumerate the files yourself. A relative path is rejected
outright rather than resolved, because it would silently bind to the *runner's*
cwd, which is never the caller's.

For `pi`, the staged temp dir is the agent's working directory and it gets
**copies**, listed to it as **staged basenames only** — the tool never puts the
original absolute paths in front of pi. `stage_copies` disambiguates a basename
collision with a numeric prefix in input order, so passing `…/a/mod.rs` and
`…/b/mod.rs` gives pi `mod.rs` and `2-mod.rs`, and its answer cites those
names. Map them back yourself; if you name the originals in your own prompt to
help it, understand that you have then told pi where they came from, since
`req.prompt` is interpolated verbatim.

Read that as **negative space, not a sandbox.** The two mechanisms are a cwd
and a read-only tool allowlist (`--tools read,grep,find,ls`) — neither is a
path jail, those tools accept absolute paths, and the process inherits the
runner's full environment. The allowlist is also a *pi-side* flag this repo
does not enforce; what actually protects your files is that pi only ever gets
copies. Real confinement primitives exist here — `confine_source_path` in
`mcp/plan_library.rs`, `is_scoped` in `mcp/probe_executor.rs` — and the
subagent path calls neither. What holds is that pi is not *given* anything
outside the staged set and has no relative path that resolves out of it, so
"find the bug and cite line numbers" works while "check how this is called"
does not unless the callers are in `file_refs` too.

`deepseek` is the opposite on this point: `compose_inline_prompt` labels every
inlined section with the ref's **original absolute path**, so those paths do go
to the model. A ref that is not valid UTF-8 fails the call with a "handles text
files only" error. Both bounds are strict `>`, so a file of exactly 256KB and a
set totalling exactly 1MB both pass. The budgets are checked in input order as
the list is walked, and they report differently: the per-file error names the
offending file, while the total error (spelled `1024KB` in the message) names
none — it fires on whichever ref pushed the running sum over, not necessarily
the largest.

## Provenance

Merged 2026-08-21 from `/analyze-pi` + `/analyze-deepseek`, which were one
command described twice: same MCP tool, same argument list, same fallback,
differing only in the `provider` value — which the tool takes as a required enum
argument, so the split encoded a parameter as a filename.

The contract above is verified against the implementing code, not the tool's
own description string: `subagent_tool_entry` in
`qontinui-runner/src-tauri/src/bin/wrappers_mcp.rs` for the schema, and
`execute_pi` / `execute_deepseek` in
`qontinui-runner/src-tauri/src/subagent/mod.rs` for the per-provider
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

**Corrected again 2026-08-25**, by the same method and with the same lesson.
Reading one layer produced four more claims that the layer below refutes:

- *"Both run the same model."* Shipped by #327 — the only one of these that
  reached `main`. `PiCliSettings::model` defaults to `None` and
  `build_flag_args` omits `--model` when it is unset, so pi picks its own
  default for the `deepseek` provider while the other path pins
  `openai_compatible.model`. The conclusion (`provider` is not a model
  selector) survives; the reason did not.
- *"`openai_compatible.timeout_seconds` is the deepseek budget."* It is the
  budget of **one attempt**; `retry_with_backoff_tracked` in `routing.rs` runs
  up to `MAX_AI_RETRIES = 3` more. Stopping at `openai_compat.rs` understates
  the real ceiling by about 4x — and the first attempt to write the ceiling down
  credited the circuit breaker with shortening a retry sequence it is only ever
  consulted before.
- *"Both reach the same DeepSeek account."* Written while fixing the bullet
  above, and caught in the same pass. `deepseek_key_env` is a *fallback* under
  three guards, and the two paths resolve the key in **opposite order** — so a
  `DEEPSEEK_API_KEY` in the environment that differs from the keychain entry
  splits them, invisibly to the runner.
  Fixing an overstatement with a second overstatement is the characteristic
  failure here.
- *"pi cannot reach anything you did not pass."* `--tools read,grep,find,ls`
  plus a cwd is a tool allowlist, not a path jail. This repo has real
  confinement primitives — `confine_source_path`, `is_scoped` — and the
  subagent path calls neither. pi is not *given* the wider tree; it is not
  *prevented* from reading it.

Counting the one #326 nearly shipped, that is five inversions across three
passes, three of them in this one, and every case has the same shape: a layer that reads like a guarantee, believed
without following the call to the thing that finally does the work. The rule
this doc keeps rediscovering — where a name, a comment, or a settings field
asserts a guarantee, find the mechanism that enforces it before writing it
down. `deepseek_key_env` is named like a coupling and is a fallback;
`timeout_seconds` is named like a budget and is a per-attempt bound; the pi
tool allowlist is named like a sandbox and is a flag.

**A sixth, later the same day**, in the other sentence #327 shipped — the one
pass above had corrected its model claim and left this one standing:
*"the pi CLI enforces it."* It does not, because nothing passes a timeout to pi
at all. `build_flag_args` emits `-p`, `--no-session`, `--provider`, `--model`
and `--tools`, and no other flag; the budget is a runner-side deadline in
`execute_argv` that kills the process tree. Same shape again — `execute_pi`
assigning `settings.timeout_seconds` reads like configuring the child, and
configures only the parent's own kill clock. The same sentence cited
`default_pi_cli_timeout()` as the thing being overridden, when that is the
serde default of an operator-editable persisted field: what a subagent run
actually shortens is whatever is stored, and 600 is merely its value on a
default install.

The title carried the same defect in the one line the command list shows.
`(pi or DeepSeek)` named a model as one of two alternatives while the body
said `provider` never selects a model — so the index entry taught the
misreading the document exists to correct. Both #327 and the pass above
rewrote the body and neither touched line 1.

Pre-PR review then caught three more in the draft of *this* pass, which is
the point worth keeping: correcting an overstatement kept reaching for
another one. "Kills the pid and its process tree" is Windows-only and
best-effort by the implementation's own admission. "There is no pi-side
cooperative deadline" inferred the layer below from the absence of a flag in
the layer above — the same unverifiable move this section exists to catalogue.
And "shortens the effective setting" contradicted its own premise: once you
grant that the persisted value need not be 600, replacing a stored 120s with
the 300s default *raises* the budget. The assignment replaces; the direction
is whatever the two numbers happen to be.

The same review found the mis-attribution had already propagated: the
`timeout_secs: 0` paragraph called the runner's own hardcoded 600s fallback
"pi's", four lines under the sentence establishing that pi is passed no
timeout at all. Fixed here, and worth noting it is a *third* 600 —
`run_pi_cli_in_dir`'s literal and `default_pi_cli_timeout()` are unrelated
constants that are merely equal.
