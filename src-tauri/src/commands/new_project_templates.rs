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
//!   state-machine IR at build time).
//!
//! Files are parameterized on `{{name}}` (the project/repo name) and
//! `{{app_id}}` (the derived slug used for the app registry and package
//! name) via plain string replacement. Both values are validated upstream
//! (`new_project.rs`) to safe character sets (`[A-Za-z0-9._-]` /
//! `[a-z0-9-]`), so no JSON/HTML escaping is needed here.

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
/// rather than a source file; the line is inert for the `empty` template.
const GITIGNORE: &str = "node_modules/
dist/
src/state-machine.derived.json
target/
.env*
__pycache__/
.DS_Store
";

const EMPTY_README: &str = "# {{name}}
";

const REACT_README: &str = r#"# {{name}}

A React + UI Bridge app scaffolded by Qontinui.

## Development

```bash
npm install
npm run dev
```

## Spec-driven development

Specs live in `specs/pages/<id>/`. Author states with `<State>` and
transitions with `<TransitionTo>` from `@qontinui/ui-bridge`; the
`uiBridgeIRPlugin` in `vite.config.ts` compiles those annotations into
`src/state-machine.derived.json` on every build.
"#;

/// Dependency versions pinned to the caret ranges the runner itself ships
/// with (see the repo-root `package.json`) so a fresh scaffold and the
/// runner agree on the UI Bridge contract.
const REACT_PACKAGE_JSON: &str = r#"{
  "name": "{{app_id}}",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "preview": "vite preview"
  },
  "dependencies": {
    "@qontinui/ui-bridge": "^0.21.1",
    "react": "^19.2.4",
    "react-dom": "^19.2.4"
  },
  "devDependencies": {
    "@qontinui/ui-bridge-auto": "^0.1.7",
    "@types/react": "^19.2.14",
    "@types/react-dom": "^19.2.3",
    "@vitejs/plugin-react": "^6.0.0",
    "typescript": "^6.0.2",
    "vite": "^8.1.3"
  }
}
"#;

const REACT_VITE_CONFIG: &str = r#"import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { uiBridgeIRPlugin } from "@qontinui/ui-bridge-auto/ir-builder";

export default defineConfig({
  plugins: [
    react(),
    uiBridgeIRPlugin({
      documentId: "{{app_id}}",
      documentName: "{{name}} State Machine",
    }),
  ],
});
"#;

const REACT_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "useDefineForClassFields": true,
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "skipLibCheck": true,
    "noEmit": true,
    "isolatedModules": true,
    "resolveJsonModule": true
  },
  "include": ["src"]
}
"#;

const REACT_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>{{name}}</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
"#;

const REACT_MAIN_TSX: &str = r#"import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { UIBridgeProvider } from "@qontinui/ui-bridge";

import App from "./App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <UIBridgeProvider features={{ control: true }}>
      <App />
    </UIBridgeProvider>
  </StrictMode>,
);
"#;

const REACT_APP_TSX: &str = r#"import { State } from "@qontinui/ui-bridge";

export default function App() {
  return (
    <State
      id="home"
      name="Home"
      metadata={{ description: "Starter home page for {{name}}" }}
    >
      <main>
        <h1>{{name}}</h1>
        <p>
          This page is a UI Bridge state. Author more states with{" "}
          <code>&lt;State&gt;</code> and connect them with{" "}
          <code>&lt;TransitionTo&gt;</code> — the ui-bridge-auto Vite plugin
          compiles them into the app&apos;s state-machine IR on every build.
        </p>
      </main>
    </State>
  );
}
"#;

const REACT_CLAUDE_MD: &str = r#"# {{name}} — Claude Code Guidelines

This project uses spec-driven development with the Qontinui UI Bridge.

- Specs live in `specs/pages/<id>/`.
- Author states with `<State>` and transitions with `<TransitionTo>` from
  `@qontinui/ui-bridge`; the ui-bridge-auto Vite plugin (`uiBridgeIRPlugin`
  in `vite.config.ts`) derives the state-machine IR at build time into
  `src/state-machine.derived.json`.
- Keep every page reachable through declared transitions so automated spec
  checks can walk the whole app.
"#;

/// A real CI check for the scaffolded repo.
///
/// A repo with no workflow files at all is a repo where no check can ever be
/// red: coord's merge predicate has an arm that admits a PR carrying zero
/// check rows when the default branch is positively established to hold no
/// workflows, so a freshly scaffolded owner repo would auto-merge every PR
/// against nothing. Shipping this file closes that composition — once `main`
/// carries a workflow, that arm declines and zero-check PRs block.
///
/// Four measured constraints, do not "improve" past them:
/// - `npm install`, never `npm ci` — the template ships no `package-lock.json`
///   and `npm ci` fails `EUSAGE` on a bare scaffold. For the same reason
///   `setup-node` carries no `cache: npm`, which keys on a lockfile.
/// - `npm run build` is the whole check: the template's `build` script is
///   already `tsc --noEmit && vite build`, so it covers typecheck and build.
/// - No tree-cleanliness step: `vite build` writes
///   `src/state-machine.derived.json`, so one would fail by construction.
/// - No warnings-as-errors: the build legitimately warns (a `crypto`
///   browser-externalization notice, a direct-`eval` advisory inside
///   ui-bridge's dist, a chunk over vite's 500 kB hint) and still exits 0.
///
/// Deliberately contains no `${{ ... }}` GitHub Actions expressions, keeping
/// this file's `{{name}}`/`{{app_id}}` templating and Actions' own out of the
/// same document.
///
/// `node-version: '24'` is the version the green run was actually measured on,
/// and it is not arbitrary: `vite@8` and `@vitejs/plugin-react@6` both declare
/// `engines.node = "^20.19.0 || >=22.12.0"`, so a bare `'20'` would put a
/// scaffolded repo's CI on an end-of-life major sitting exactly on that floor —
/// green today, red on the next drift. Raise this with the template's
/// dependency floors; never lower it below `22.12`.
const REACT_CI_WORKFLOW: &str = r#"name: CI
on:
  push:
    branches: [main]
  pull_request:
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: '24'
      - run: npm install
      - run: npm run build
"#;

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
                content: sub(REACT_PACKAGE_JSON),
            },
            TemplateFile {
                path: "vite.config.ts",
                content: sub(REACT_VITE_CONFIG),
            },
            TemplateFile {
                path: "tsconfig.json",
                content: REACT_TSCONFIG.to_string(),
            },
            TemplateFile {
                path: "index.html",
                content: sub(REACT_INDEX_HTML),
            },
            TemplateFile {
                path: "src/main.tsx",
                content: REACT_MAIN_TSX.to_string(),
            },
            TemplateFile {
                path: "src/App.tsx",
                content: sub(REACT_APP_TSX),
            },
            TemplateFile {
                path: "specs/.gitkeep",
                content: String::new(),
            },
            TemplateFile {
                path: "README.md",
                content: sub(REACT_README),
            },
            TemplateFile {
                path: ".gitignore",
                content: GITIGNORE.to_string(),
            },
            TemplateFile {
                path: "CLAUDE.md",
                content: sub(REACT_CLAUDE_MD),
            },
            TemplateFile {
                path: ".github/workflows/ci.yml",
                content: REACT_CI_WORKFLOW.to_string(),
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
mod tests {
    use super::*;

    #[test]
    fn substitute_replaces_all_occurrences_of_both_params() {
        let out = substitute("{{name}} + {{app_id}} / {{name}}", "My.App", "my-app");
        assert_eq!(out, "My.App + my-app / My.App");
    }

    #[test]
    fn substitute_leaves_jsx_braces_alone() {
        let out = substitute("metadata={{ description: \"{{name}}\" }}", "demo", "demo");
        assert_eq!(out, "metadata={{ description: \"demo\" }}");
    }

    #[test]
    fn empty_template_has_readme_and_gitignore() {
        let files = template_files(TEMPLATE_EMPTY, "proj", "proj").unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path).collect();
        assert_eq!(paths, vec!["README.md", ".gitignore"]);
        assert_eq!(files[0].content, "# proj\n");
        assert!(files[1].content.contains("node_modules/"));
    }

    #[test]
    fn react_template_is_fully_parameterized() {
        let files = template_files(TEMPLATE_REACT_UI_BRIDGE, "My-App", "my-app").unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path).collect();
        for expected in [
            "package.json",
            "vite.config.ts",
            "tsconfig.json",
            "index.html",
            "src/main.tsx",
            "src/App.tsx",
            "specs/.gitkeep",
            "README.md",
            ".gitignore",
            "CLAUDE.md",
            ".github/workflows/ci.yml",
        ] {
            assert!(paths.contains(&expected), "missing {}", expected);
        }
        for f in &files {
            assert!(
                !f.content.contains("{{name}}") && !f.content.contains("{{app_id}}"),
                "unsubstituted placeholder in {}",
                f.path
            );
        }
        let pkg = files.iter().find(|f| f.path == "package.json").unwrap();
        assert!(pkg.content.contains("\"name\": \"my-app\""));
        assert!(pkg.content.contains("@qontinui/ui-bridge"));
        assert!(pkg.content.contains("@qontinui/ui-bridge-auto"));
        let vite = files.iter().find(|f| f.path == "vite.config.ts").unwrap();
        assert!(vite
            .content
            .contains("import { uiBridgeIRPlugin } from \"@qontinui/ui-bridge-auto/ir-builder\""));
        assert!(vite.content.contains("documentId: \"my-app\""));
        let app = files.iter().find(|f| f.path == "src/App.tsx").unwrap();
        assert!(app.content.contains("<State"));
        assert!(app.content.contains("id=\"home\""));
        // The CI workflow is what makes a scaffolded repo's PRs blockable at
        // all; its measured constraints are pinned here so a later "cleanup"
        // cannot quietly break a bare scaffold's build.
        let ci = files
            .iter()
            .find(|f| f.path == ".github/workflows/ci.yml")
            .unwrap();
        assert!(ci.content.contains("- run: npm install"));
        assert!(ci.content.contains("- run: npm run build"));
        // No lockfile ships, so `npm ci` and setup-node's lockfile-keyed cache
        // both fail on a bare scaffold.
        assert!(!ci.content.contains("npm ci"));
        assert!(!ci.content.contains("cache:"));
        // `vite build` writes src/state-machine.derived.json, so a
        // tree-cleanliness step would fail by construction.
        assert!(!ci.content.contains("git diff"));
        // Keep Actions' expression syntax out of a file this module templates.
        assert!(!ci.content.contains("${{"));
    }

    #[test]
    fn unknown_template_is_rejected() {
        assert!(template_files("angular", "x", "x").is_err());
    }

    #[test]
    fn project_type_and_manifest_mapping() {
        assert_eq!(project_type_for_template(TEMPLATE_REACT_UI_BRIDGE), "react");
        assert_eq!(project_type_for_template(TEMPLATE_EMPTY), "empty");
        assert_eq!(
            manifest_for_template(TEMPLATE_REACT_UI_BRIDGE),
            "package.json"
        );
        assert_eq!(manifest_for_template(TEMPLATE_EMPTY), "README.md");
    }
}
