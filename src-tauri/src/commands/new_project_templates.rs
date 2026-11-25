//! Embedded project templates for the New Project flow.
//!
//! Templates are compiled into the binary as string constants — scaffolding
//! never touches the network. Two templates exist:
//!
//! - `empty` — README + .gitignore, the minimum viable git project.
//! - `react-ui-bridge` — the product-thesis default: a Vite React TS app
//!   wired for spec-driven development with `@qontinui/ui-bridge` (runtime
//!   `<State>`/`<TransitionTo>` annotations) and `@qontinui/ui-bridge-auto`
//!   (the `uiBridgeIRPlugin` Vite plugin that compiles annotations into the
//!   state-machine IR at build time), and a design that is compiled rather
//!   than prose: a `design/language.dtcg.json` source, the committed
//!   `design-kit` outputs under `src/design/`, a state-complete component kit,
//!   a specimen page and a generated component index in `CLAUDE.md`. Its file
//!   constants live in [`design`].
//!
//! Files are parameterized on `{{name}}` (the project/repo name) and
//! `{{app_id}}` (the derived slug used for the app registry and package
//! name) via plain string replacement. Both values are validated upstream
//! (`new_project.rs`) to safe character sets (`[A-Za-z0-9._-]` /
//! `[a-z0-9-]`), so no JSON/HTML escaping is needed here.

mod design;

/// One file to write into the new project, path relative to the project root
/// (always forward-slash separated; the writer converts to native paths).
#[derive(Debug, Clone)]
pub struct TemplateFile {
    pub path: &'static str,
    pub content: String,
}

/// Template identifiers accepted on the wire.
pub const TEMPLATE_EMPTY: &str = "empty";
pub const TEMPLATE_REACT_UI_BRIDGE: &str = "react-ui-bridge";

/// Shared .gitignore for both templates. `src/state-machine.derived.json` is
/// written by `vite build` (the `uiBridgeIRPlugin`), so it is a build output
/// rather than a source file; `state-coverage.json` is the output of a manual
/// `design-kit check-states` run. Both lines are inert for the `empty` template.
const GITIGNORE: &str = "node_modules/
dist/
src/state-machine.derived.json
target/
.env*
__pycache__/
.DS_Store
state-coverage.json
";

const EMPTY_README: &str = "# {{name}}
";

/// Substitute `{{name}}` / `{{app_id}}` placeholders in a template body.
pub fn substitute(raw: &str, name: &str, app_id: &str) -> String {
    raw.replace("{{name}}", name).replace("{{app_id}}", app_id)
}

/// Return the file set for a template, fully parameterized. Errors on an
/// unknown template id (the caller surfaces this at validation time).
pub fn template_files(
    template: &str,
    name: &str,
    app_id: &str,
) -> Result<Vec<TemplateFile>, String> {
    let sub = |raw: &str| substitute(raw, name, app_id);
    match template {
        TEMPLATE_EMPTY => Ok(vec![
            TemplateFile {
                path: "README.md",
                content: sub(EMPTY_README),
            },
            TemplateFile {
                path: ".gitignore",
                content: GITIGNORE.to_string(),
            },
        ]),
        TEMPLATE_REACT_UI_BRIDGE => Ok(vec![
            TemplateFile {
                path: "package.json",
                content: sub(design::PACKAGE_JSON),
            },
            TemplateFile {
                path: "vite.config.ts",
                content: sub(design::VITE_CONFIG),
            },
            TemplateFile {
                path: "tsconfig.json",
                content: design::TSCONFIG.to_string(),
            },
            TemplateFile {
                path: "index.html",
                content: sub(design::INDEX_HTML),
            },
            TemplateFile {
                path: "specimen.html",
                content: design::SPECIMEN_HTML.to_string(),
            },
            TemplateFile {
                path: "src/main.tsx",
                content: design::MAIN_TSX.to_string(),
            },
            TemplateFile {
                path: "src/App.tsx",
                content: sub(design::APP_TSX),
            },
            TemplateFile {
                path: "src/index.css",
                content: design::INDEX_CSS.to_string(),
            },
            TemplateFile {
                path: "design/language.dtcg.json",
                content: design::LANGUAGE_SOURCE.to_string(),
            },
            TemplateFile {
                path: "src/design/tokens.css",
                content: design::TOKENS_CSS.to_string(),
            },
            TemplateFile {
                path: "src/design/tokens.ts",
                content: design::TOKENS_TS.to_string(),
            },
            TemplateFile {
                path: "src/design/theme.css",
                content: design::THEME_CSS.to_string(),
            },
            TemplateFile {
                path: "src/design/compile-report.json",
                content: design::COMPILE_REPORT.to_string(),
            },
            TemplateFile {
                path: "src/design/design-budgets.json",
                content: design::DESIGN_BUDGETS.to_string(),
            },
            TemplateFile {
                path: "src/design/visual-system.thresholds.json",
                content: design::THRESHOLDS.to_string(),
            },
            TemplateFile {
                path: "src/design/app.styleguide.uibridge.json",
                content: design::STYLE_GUIDE.to_string(),
            },
            TemplateFile {
                path: "specs/.gitkeep",
                content: String::new(),
            },
            TemplateFile {
                path: "README.md",
                content: sub(design::README),
            },
            TemplateFile {
                path: ".gitignore",
                content: GITIGNORE.to_string(),
            },
            TemplateFile {
                path: "CLAUDE.md",
                content: sub(design::CLAUDE_MD),
            },
            TemplateFile {
                path: ".github/workflows/ci.yml",
                content: design::CI_WORKFLOW.to_string(),
            },
        ]),
        other => Err(format!(
            "Unknown template '{}': expected '{}' or '{}'",
            other, TEMPLATE_EMPTY, TEMPLATE_REACT_UI_BRIDGE
        )),
    }
}

/// `SavedProject.project_type` value for a template.
pub fn project_type_for_template(template: &str) -> &'static str {
    match template {
        TEMPLATE_REACT_UI_BRIDGE => "react",
        _ => "empty",
    }
}

/// `SavedProject.manifest` value for a template — the file that identifies
/// the project type, mirroring what the setup-wizard scanner would detect.
pub fn manifest_for_template(template: &str) -> &'static str {
    match template {
        TEMPLATE_REACT_UI_BRIDGE => "package.json",
        _ => "README.md",
    }
}

#[cfg(test)]
mod tests;
