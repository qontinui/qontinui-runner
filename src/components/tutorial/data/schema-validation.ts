/**
 * Schema Validation Tutorial (Informational)
 *
 * Explains the 7-layer structured output validation system: what problem it
 * solves, how each layer works, what the UI surfaces, and how the layers
 * connect to form a reliability stack for multi-agent AI output.
 */

import type { Tutorial } from "../../../types/tutorial";

export const schemaValidationTutorial: Tutorial = {
  id: "schema-validation",
  title: "Structured Output & Schema Validation",
  description:
    "Understand the 7-layer validation stack that ensures AI agents produce well-formed, machine-readable output. Covers schema derivation, runtime validation, retry feedback, constrained decoding, inter-agent contracts, compliance scoring, and cross-language type generation.",
  duration: "14 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "gui-automation",
  category: "AI Architecture",
  tags: ["featured", "schema", "validation", "structured-output", "agents", "reliability"],
  prerequisites: ["workflow-execution"],
  learningObjectives: [
    "Understand why LLM output needs schema validation and what goes wrong without it",
    "Know how JSON Schemas are derived automatically from Rust types (single source of truth)",
    "Understand runtime validation, type coercion, and the retry-with-feedback loop",
    "Know how constrained decoding enforces structure at the LLM backend level",
    "Read the Schema column in the Pipeline Trace table and interpret compliance indicators",
    "Understand how schema compliance feeds into the meta-optimizer for model selection",
  ],
  steps: [
    {
      id: "the-problem",
      title: "The Problem: LLMs Don't Reliably Produce JSON",
      content: `When an AI agent runs a workflow step, the orchestrator needs structured data back — not free-form text. It needs to know: did verification pass? What files changed? What's the confidence level?

**The challenge:** LLMs are probabilistic text generators. Even when instructed to output JSON, they often:
- Return extra prose before or after the JSON block
- Use strings where numbers are expected (\`"42"\` instead of \`42\`)
- Omit required fields or invent fields not in the schema
- Produce subtly invalid structures that parse as JSON but break downstream logic

Without validation, these malformed outputs cause **silent failures** — the orchestrator continues with bad data, leading to cascading errors that are hard to diagnose.`,
      tips: [
        "This is a universal problem across all LLM-powered systems, not specific to any one model",
        "The error rate varies by model: smaller/local models fail more often than frontier models",
        "Even frontier models occasionally produce malformed output, especially for complex nested schemas",
      ],
      estimatedDuration: 1,
    },
    {
      id: "layer1-schema-first",
      title: "Layer 1: Schema-First Type System",
      content: `The foundation is **automatic schema generation from Rust types**. Every structured output type (WorkerOutput, StructuredSignal, ToolOutput, etc.) derives a JSON Schema automatically via the \`schemars\` crate.

This means:
- **One source of truth** — the Rust struct IS the schema. No hand-written JSON Schema that drifts out of sync.
- **Schema Registry** — \`schema_as_json<T>()\` returns the cached JSON Schema for any type. \`prompt_instructions_for<T>()\` generates LLM-readable format instructions from the schema.
- **Compile-time guarantees** — if a field is added or renamed in Rust, the schema updates automatically. Forgetting to update a hand-written schema is impossible.`,
      details: `**How it works under the hood:**

The \`#[derive(JsonSchema)]\` macro (from the \`schemars\` crate) generates a \`json_schema()\` method on the type. At runtime, the schema registry calls this once, caches the result as a \`serde_json::Value\`, and serves it for all subsequent requests.

The prompt instruction generator walks the schema and produces human-readable field descriptions:
\`\`\`
Required field "confidence" (number): A value between 0.0 and 1.0
Required field "findings" (array of objects): Each with "type", "message", "severity"
\`\`\`

This replaces the old hand-written \`worker_output_instructions()\` function that had to be manually kept in sync with the struct definition.`,
      tips: [
        "The schema registry uses a global cache — schemas are generated once and reused",
        'schemars supports #[schemars(description = "...")] for field-level docs that appear in the schema',
      ],
      estimatedDuration: 2,
    },
    {
      id: "layer2-runtime-validation",
      title: "Layer 2: Runtime Validation Engine",
      content: `Once we have a schema, the **SchemaValidator** checks any JSON value against it at runtime using the \`jsonschema\` crate.

Three components work together:

**Validator** — \`validate(value)\` returns a list of specific errors: "missing field 'confidence'", "expected number, got string at /findings/0/severity".

**Coercion Engine** — Before rejecting output, attempts safe type conversions based on a **CoercionPolicy**:
- \`"42"\` (string) -> \`42\` (number) when schema expects number
- \`"true"\` (string) -> \`true\` (bool)
- \`42\` (number) -> \`true\` (bool, non-zero = true)
- \`"hello"\` (bare value) -> \`["hello"]\` (array) when schema expects array

**Feedback Generator** — Formats validation errors into a numbered, LLM-readable list for retry prompts.`,
      details: `**Coercion Policies:**

- **Strict** — No coercion. The value must match the schema exactly. Use for inter-agent contracts where precision matters.
- **Lenient** — Apply safe coercions (string-to-number, string-to-bool, etc.). Default for LLM output parsing. Fixes the most common LLM formatting mistakes.
- **Off** — Skip validation entirely. Used as an escape hatch when performance matters more than correctness.

Both \`parse_worker_output()\` and \`try_parse_json_summary()\` — the two main output parsing paths — now validate and coerce before deserializing.`,
      tips: [
        "Coercion happens in-place on a cloned value, so the original is never modified",
        "The validator is compiled from the schema once and reused — validation itself is fast",
      ],
      estimatedDuration: 2,
    },
    {
      id: "layer3-retry-feedback",
      title: "Layer 3: Retry with Validation Feedback",
      content: `When validation fails even after coercion, the system **asks the LLM to fix its own output**.

The retry loop works like this:
1. LLM produces output -> validation fails
2. Build a **retry prompt** containing the specific errors: *"Error 1: Missing required field 'confidence'. Error 2: Field 'severity' expected number but got string."*
3. On the **first retry**, include the full JSON Schema so the LLM has the complete spec
4. On subsequent retries, omit the schema (the LLM already has it in context) and just send errors
5. After **max 2 retries**, accept whatever the LLM produced (with warnings) or fall back to defaults

This is implemented in \`validate_and_parse<T>()\` and wired through \`StructuredOutputMiddleware\` in the AI provider chain.`,
      tips: [
        "Including the schema only on retry #1 saves tokens on subsequent attempts",
        "Max retries defaults to 2 — enough to fix most issues without excessive latency",
        "The middleware logs a warning when a schema isn't registered for a type (indicates a gap in coverage)",
      ],
      estimatedDuration: 1,
    },
    {
      id: "layer4-constrained-decoding",
      title: "Layer 4: Constrained Decoding",
      content: `Some LLM backends can enforce output structure **during generation**, not just after. This is called **constrained decoding** — the model physically cannot produce tokens that violate the schema.

The system adapts the schema format per backend:

- **OpenAI / Azure OpenAI** — Sends \`response_format: { type: "json_schema", json_schema: {...} }\`
- **Gemini / Google AI** — Sends \`responseMimeType: "application/json"\` + \`responseSchema\`
- **Local models (Ollama, vLLM)** — Converts JSON Schema to **GBNF grammar** (a context-free grammar format that llama.cpp uses to constrain token sampling)
- **Claude** — Uses \`tool_use\` with \`input_schema\` for server-side enforcement

When constrained decoding is available, layers 2-3 (validation + retry) serve as a safety net. When it's not available, they're the primary defense.`,
      details: `**GBNF Grammar Generation:**

For local models, the system walks the JSON Schema and generates GBNF rules:
- Objects become rules with required/optional property lists
- Enums become alternation rules (\`"low" | "medium" | "high"\`)
- Arrays get item type rules with repetition
- Nested objects and \`$ref\` references are resolved recursively
- \`oneOf\` becomes GBNF alternation

This means even a local 7B model running on Ollama gets schema enforcement comparable to OpenAI's server-side feature.`,
      tips: [
        "GBNF stands for GGML BNF — it's the grammar format used by llama.cpp and derivatives",
        "Constrained decoding can slightly slow generation but eliminates retry overhead entirely",
        "The backend is auto-detected from the provider configuration — no manual setup needed",
      ],
      estimatedDuration: 2,
    },
    {
      id: "layer5-contracts",
      title: "Layer 5: Inter-Agent Contracts",
      content: `In a multi-agent pipeline (Spec Analyst -> Locator -> Implementer -> Verifier), each agent's output is the next agent's input. A **typed contract** at each boundary ensures one agent's malformed output doesn't cascade.

Four contract types define what each agent must produce:
- **SpecAnalystOutput** — parsed requirements, acceptance criteria, constraints
- **LocatorOutput** — file paths, relevance scores, code context
- **ImplementerOutput** — file changes, reasoning, confidence
- **VerifierOutput** — pass/fail per criterion, evidence, regression flags

The **ContractValidator** pre-compiles a schema validator for each role at pipeline start. At each phase boundary, it validates the agent's output. Failures are **logged as warnings** but don't block execution — this is intentional. The pipeline should degrade gracefully, not crash because one agent returned slightly off-schema output.`,
      tips: [
        "Contract validation is non-blocking by design — strict enforcement would make the system fragile",
        "The contract types derive JsonSchema just like all other output types",
        "Contract violations are tracked and feed into schema compliance scoring (Layer 6)",
      ],
      estimatedDuration: 1,
    },
    {
      id: "layer6-compliance-scoring",
      title: "Layer 6: Schema Compliance as a Meta-Metric",
      content: `Every validation attempt is tracked and scored. The **schema compliance score** is a 0.0-1.0 metric that feeds into the meta-optimizer:

| Score | Meaning |
|-------|---------|
| **1.0** | Valid on first attempt — no coercion or retry needed |
| **0.8** | Valid after type coercion (e.g., string -> number) |
| **0.5** | Valid after 1 retry with feedback |
| **0.2** | Valid after 2+ retries |
| **0.0** | Never produced valid output |

This data is stored per pipeline agent trace and aggregated across runs. Over time, it reveals:
- Which **models** have the worst compliance (informing model selection)
- Which **agent roles** produce the most schema violations (informing prompt tuning)
- Whether compliance is **trending up or down** (regression detection)`,
      details: `**How it feeds into the meta-optimizer:**

Schema compliance is registered as an \`AgenticMetric\` variant alongside existing metrics like task completion, step efficiency, and tool correctness. The \`compute_deterministic()\` function calculates it from pipeline traces stored in the database.

The scoring ladder is intentional — it rewards first-attempt success heavily (1.0) and penalizes retries progressively. This means the meta-optimizer will naturally prefer models that get it right on the first try, reducing both latency and token cost.`,
      estimatedDuration: 1,
    },
    {
      id: "ui-metrics-panel",
      title: "Where You See It: Agentic Metrics Panel",
      content: `The **Agentic Metrics Panel** (in Meta-Optimizer > Progress) shows schema compliance as one row among all agentic metrics.

It displays:
- A **score bar** showing the 30-day average compliance rate
- The **number of runs** scored
- A **trend sparkline** in the composite score chart (schema compliance contributes to the composite)

When you select a specific task run, the per-run breakdown shows the exact compliance score for that execution.

You don't need to do anything special to see this — it appears automatically once workflows run with the validation system active.`,
      targetElement: {
        selector: "agentic-metrics-panel",
        highlightType: "border",
        position: "top",
      },
      tips: [
        "Schema Compliance appears alongside Task Completion, Step Efficiency, Tool Correctness, etc.",
        "The composite score is a weighted average — schema compliance contributes to the overall health signal",
        "Color coding: green (>80%), yellow (60-80%), orange (40-60%), red (<40%)",
      ],
      estimatedDuration: 1,
    },
    {
      id: "ui-trace-table",
      title: "Where You See It: Pipeline Trace Table",
      content: `The **Pipeline Trace Table** (in Generator Evaluation > Pipeline Inspector) shows per-agent schema compliance for each pipeline execution.

The **Schema column** shows one of three indicators per agent:
- **Green checkmark** — output was valid on first attempt
- **Yellow R1/R2** — output required 1 or 2 retries to become valid
- **Red X** — output never became valid (fell back to defaults)
- **Gray dash** — no schema validation data (older runs or unregistered types)

**Click a row to expand it.** If validation errors occurred, you'll see a red-tinted **Validation Errors** panel showing the specific schema violations — exactly what was sent back to the LLM in the retry prompt.`,
      targetElement: {
        selector: "pipeline-trace-table",
        highlightType: "spotlight",
        position: "top",
      },
      tips: [
        "The Schema column helps you spot which agents in a pipeline are unreliable",
        "If you see consistent R1/R2 for a specific agent type, its prompt may need schema instructions added",
        "The validation error summary is the same text the retry system sent to the LLM",
      ],
      estimatedDuration: 1,
    },
    {
      id: "layer7-cross-language",
      title: "Layer 7: Cross-Language Schema Generation",
      content: `The Rust types are the source of truth, but the frontend is TypeScript and scripts may be Python. The **cross-language generation pipeline** keeps all three in sync:

1. \`cargo run --bin export_schemas\` — Generates JSON Schema files from 12 core Rust types (WorkerOutput, AppEvent, FlowEvent, ToolOutput, etc.)
2. \`scripts/generate_types.sh\` — Converts those schemas to:
   - **TypeScript** types via \`json-schema-to-typescript\`
   - **Python** dataclasses via \`datamodel-codegen\`

This means the \`PipelineAgentTrace\` type you see in the frontend (with its \`schema_valid_first_attempt\`, \`validation_retries\`, and \`validation_error_summary\` fields) was derived from the same Rust struct that the backend validates against.`,
      tips: [
        "Run generate_types.sh after changing any schema-deriving Rust type",
        "The export binary outputs to stdout by default; use --pretty for readable JSON",
        "12 types are currently exported — more can be added by extending schema_export.rs",
      ],
      estimatedDuration: 1,
    },
    {
      id: "how-layers-connect",
      title: "How the Layers Connect",
      content: `The 7 layers form a **defense-in-depth stack**. Each layer addresses a different failure mode:

**Prevention** (before the LLM responds):
- Layer 1 (Schema) generates instructions telling the LLM what to produce
- Layer 4 (Constrained Decoding) prevents invalid tokens at generation time

**Detection + Recovery** (after the LLM responds):
- Layer 2 (Validation) catches what got through
- Layer 3 (Retry) gives the LLM a chance to self-correct

**Boundaries** (between agents):
- Layer 5 (Contracts) prevents cascade failures across pipeline phases

**Learning** (over time):
- Layer 6 (Compliance Scoring) tracks reliability and informs model selection
- Layer 7 (Cross-Language) ensures the whole stack speaks the same type language

The key design principle: **graceful degradation**. No single layer failing should crash a workflow. Constrained decoding is best-effort, validation falls back to coercion, retries have a cap, contracts warn but don't block.`,
      estimatedDuration: 1,
    },
    {
      id: "summary",
      title: "What You've Learned",
      content: `You now understand the structured output validation stack:

- **Schemas are derived from code**, not hand-written — they can't drift
- **Runtime validation + coercion** catches and fixes common LLM output mistakes
- **Retry with feedback** gives the LLM specific errors to fix, with the schema on first retry
- **Constrained decoding** prevents invalid output at generation time (when the backend supports it)
- **Inter-agent contracts** prevent cascading failures in multi-agent pipelines
- **Compliance scoring** tracks validation success rates as a meta-optimizer metric
- The **Schema column** in Pipeline Traces and **Schema Compliance row** in Agentic Metrics show this data in the UI

The entire system is designed to be **invisible when working** and **diagnostic when failing** — you only notice it when something goes wrong, and when it does, it tells you exactly what and where.`,
      estimatedDuration: 1,
    },
  ],
};
