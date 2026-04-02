//! Query engine for accessibility trees.
//!
//! Provides a composable, Testing Library-inspired query API for finding nodes
//! in a `UnifiedNode` tree by role, label, value, automation ID, and other
//! criteria. Inspired by `rerun-io/kittest`.

use super::model::{TriBool, UnifiedNode, UnifiedRole, UnifiedState};

/// Filter predicate for matching nodes in an accessibility tree.
#[derive(Debug, Clone)]
pub enum By {
    /// Match by exact role.
    Role(UnifiedRole),
    /// Match by any of the given roles.
    Roles(Vec<UnifiedRole>),
    /// Match by exact accessible name (label).
    Label(String),
    /// Match by substring in accessible name.
    LabelContains(String),
    /// Match by exact value.
    Value(String),
    /// Match by substring in value.
    ValueContains(String),
    /// Match by exact automation ID.
    AutomationId(String),
    /// Match by CSS/control class name.
    ClassName(String),
    /// Match by HTML tag name.
    HtmlTag(String),
    /// Match only interactive elements.
    Interactive,
    /// Match by state criteria.
    State(StateMatcher),
    /// All sub-filters must match (AND).
    And(Vec<By>),
    /// At least one sub-filter must match (OR).
    Or(Vec<By>),
    /// Invert the match.
    Not(Box<By>),
    /// Node must have an ancestor matching this filter.
    Ancestor(Box<By>),
    /// Maximum tree depth to search.
    MaxDepth(u32),
}

/// Matches specific state flags on a node.
#[derive(Debug, Clone, Default)]
pub struct StateMatcher {
    pub focused: Option<bool>,
    pub disabled: Option<bool>,
    pub hidden: Option<bool>,
    pub checked: Option<TriBool>,
    pub expanded: Option<TriBool>,
    pub selected: Option<TriBool>,
    pub pressed: Option<TriBool>,
    pub readonly: Option<bool>,
    pub editable: Option<bool>,
    pub focusable: Option<bool>,
    pub modal: Option<bool>,
}

impl StateMatcher {
    fn matches(&self, state: &UnifiedState) -> bool {
        if let Some(v) = self.focused {
            if state.is_focused != v {
                return false;
            }
        }
        if let Some(v) = self.disabled {
            if state.is_disabled != v {
                return false;
            }
        }
        if let Some(v) = self.hidden {
            if state.is_hidden != v {
                return false;
            }
        }
        if let Some(v) = self.checked {
            if state.is_checked != v {
                return false;
            }
        }
        if let Some(v) = self.expanded {
            if state.is_expanded != v {
                return false;
            }
        }
        if let Some(v) = self.selected {
            if state.is_selected != v {
                return false;
            }
        }
        if let Some(v) = self.pressed {
            if state.is_pressed != v {
                return false;
            }
        }
        if let Some(v) = self.readonly {
            if state.is_readonly != v {
                return false;
            }
        }
        if let Some(v) = self.editable {
            if state.is_editable != v {
                return false;
            }
        }
        if let Some(v) = self.focusable {
            if state.is_focusable != v {
                return false;
            }
        }
        if let Some(v) = self.modal {
            if state.is_modal != v {
                return false;
            }
        }
        true
    }
}

/// Fluent query builder for finding nodes in an accessibility tree.
///
/// # Example
/// ```ignore
/// let results = QueryBuilder::new()
///     .by_role(UnifiedRole::Button)
///     .by_label_contains("Submit")
///     .interactive()
///     .find_all(&root);
/// ```
pub struct QueryBuilder {
    filters: Vec<By>,
    limit: Option<usize>,
    case_sensitive: bool,
}

impl QueryBuilder {
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
            limit: None,
            case_sensitive: true,
        }
    }

    /// Filter by exact role.
    pub fn by_role(mut self, role: UnifiedRole) -> Self {
        self.filters.push(By::Role(role));
        self
    }

    /// Filter by any of the given roles.
    pub fn by_roles(mut self, roles: Vec<UnifiedRole>) -> Self {
        self.filters.push(By::Roles(roles));
        self
    }

    /// Filter by exact accessible name (label).
    pub fn by_label(mut self, label: impl Into<String>) -> Self {
        self.filters.push(By::Label(label.into()));
        self
    }

    /// Filter by substring in accessible name.
    pub fn by_label_contains(mut self, substr: impl Into<String>) -> Self {
        self.filters.push(By::LabelContains(substr.into()));
        self
    }

    /// Filter by exact value.
    pub fn by_value(mut self, value: impl Into<String>) -> Self {
        self.filters.push(By::Value(value.into()));
        self
    }

    /// Filter by substring in value.
    pub fn by_value_contains(mut self, substr: impl Into<String>) -> Self {
        self.filters.push(By::ValueContains(substr.into()));
        self
    }

    /// Filter by automation ID.
    pub fn by_automation_id(mut self, id: impl Into<String>) -> Self {
        self.filters.push(By::AutomationId(id.into()));
        self
    }

    /// Filter by class name.
    pub fn by_class_name(mut self, name: impl Into<String>) -> Self {
        self.filters.push(By::ClassName(name.into()));
        self
    }

    /// Filter by HTML tag name.
    pub fn by_html_tag(mut self, tag: impl Into<String>) -> Self {
        self.filters.push(By::HtmlTag(tag.into()));
        self
    }

    /// Only match interactive elements.
    pub fn interactive(mut self) -> Self {
        self.filters.push(By::Interactive);
        self
    }

    /// Filter by state criteria.
    pub fn by_state(mut self, matcher: StateMatcher) -> Self {
        self.filters.push(By::State(matcher));
        self
    }

    /// Make string matching case-insensitive.
    pub fn case_insensitive(mut self) -> Self {
        self.case_sensitive = false;
        self
    }

    /// Limit the number of results.
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Add a raw `By` filter.
    pub fn filter(mut self, by: By) -> Self {
        self.filters.push(by);
        self
    }

    /// Execute the query, returning all matching nodes.
    pub fn find_all<'a>(&self, root: &'a UnifiedNode) -> Vec<&'a UnifiedNode> {
        let mut results = Vec::new();
        let combined = if self.filters.is_empty() {
            // No filters → match everything
            None
        } else if self.filters.len() == 1 {
            Some(self.filters[0].clone())
        } else {
            Some(By::And(self.filters.clone()))
        };

        self.search(root, &combined, 0, &mut results);
        results
    }

    /// Execute the query, returning the first matching node.
    pub fn find_first<'a>(&self, root: &'a UnifiedNode) -> Option<&'a UnifiedNode> {
        let builder = Self {
            filters: self.filters.clone(),
            limit: Some(1),
            case_sensitive: self.case_sensitive,
        };
        builder.find_all(root).into_iter().next()
    }

    fn search<'a>(
        &self,
        node: &'a UnifiedNode,
        filter: &Option<By>,
        depth: u32,
        results: &mut Vec<&'a UnifiedNode>,
    ) {
        if let Some(limit) = self.limit {
            if results.len() >= limit {
                return;
            }
        }

        // Check max depth from any MaxDepth filter
        if let Some(By::And(filters)) = filter {
            for f in filters {
                if let By::MaxDepth(max) = f {
                    if depth > *max {
                        return;
                    }
                }
            }
        }
        if let Some(By::MaxDepth(max)) = filter {
            if depth > *max {
                return;
            }
        }

        let matches = match filter {
            None => true,
            Some(f) => self.node_matches(node, f),
        };

        if matches {
            results.push(node);
        }

        for child in &node.children {
            self.search(child, filter, depth + 1, results);
        }
    }

    fn node_matches(&self, node: &UnifiedNode, filter: &By) -> bool {
        match filter {
            By::Role(role) => node.role == *role,

            By::Roles(roles) => roles.contains(&node.role),

            By::Label(label) => {
                let node_name = node.name.as_deref().unwrap_or("");
                if self.case_sensitive {
                    node_name == label
                } else {
                    node_name.eq_ignore_ascii_case(label)
                }
            }

            By::LabelContains(substr) => {
                let node_name = node.name.as_deref().unwrap_or("");
                if self.case_sensitive {
                    node_name.contains(substr.as_str())
                } else {
                    node_name
                        .to_ascii_lowercase()
                        .contains(&substr.to_ascii_lowercase())
                }
            }

            By::Value(value) => {
                let node_value = node.value.as_deref().unwrap_or("");
                if self.case_sensitive {
                    node_value == value
                } else {
                    node_value.eq_ignore_ascii_case(value)
                }
            }

            By::ValueContains(substr) => {
                let node_value = node.value.as_deref().unwrap_or("");
                if self.case_sensitive {
                    node_value.contains(substr.as_str())
                } else {
                    node_value
                        .to_ascii_lowercase()
                        .contains(&substr.to_ascii_lowercase())
                }
            }

            By::AutomationId(id) => node.automation_id.as_deref() == Some(id.as_str()),

            By::ClassName(name) => node.class_name.as_deref() == Some(name.as_str()),

            By::HtmlTag(tag) => node.html_tag.as_deref() == Some(tag.as_str()),

            By::Interactive => node.is_interactive,

            By::State(matcher) => matcher.matches(&node.state),

            By::And(filters) => filters.iter().all(|f| self.node_matches(node, f)),

            By::Or(filters) => filters.iter().any(|f| self.node_matches(node, f)),

            By::Not(inner) => !self.node_matches(node, inner),

            By::Ancestor(_ancestor_filter) => {
                // Ancestor matching requires parent context which is not available
                // in a simple tree traversal. For now, always match.
                // Full ancestor matching will be added when parent pointers are available.
                true
            }

            By::MaxDepth(_) => true, // Handled in search()
        }
    }
}

impl Default for QueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::model::*;

    fn make_tree() -> UnifiedNode {
        UnifiedNode {
            ref_id: "@e0".into(),
            role: UnifiedRole::Window,
            name: Some("Test App".into()),
            value: None,
            description: None,
            bounds: None,
            state: UnifiedState::default(),
            is_interactive: false,
            level: None,
            automation_id: None,
            class_name: None,
            html_tag: None,
            url: None,
            source: NodeSource::Uia,
            platform_handle: None,
            supported_patterns: vec![],
            generation: 0,
            children: vec![
                UnifiedNode {
                    ref_id: "@e1".into(),
                    role: UnifiedRole::Button,
                    name: Some("Submit Form".into()),
                    is_interactive: true,
                    automation_id: Some("btn_submit".into()),
                    ..Default::default()
                },
                UnifiedNode {
                    ref_id: "@e2".into(),
                    role: UnifiedRole::Textbox,
                    name: Some("Email".into()),
                    value: Some("user@example.com".into()),
                    is_interactive: true,
                    state: UnifiedState {
                        is_focused: true,
                        is_editable: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                UnifiedNode {
                    ref_id: "@e3".into(),
                    role: UnifiedRole::Checkbox,
                    name: Some("Remember me".into()),
                    is_interactive: true,
                    state: UnifiedState {
                        is_checked: TriBool::True,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                UnifiedNode {
                    ref_id: "@e4".into(),
                    role: UnifiedRole::StaticText,
                    name: Some("Welcome message".into()),
                    is_interactive: false,
                    ..Default::default()
                },
            ],
        }
    }

    impl Default for UnifiedNode {
        fn default() -> Self {
            Self {
                ref_id: String::new(),
                role: UnifiedRole::Unknown,
                name: None,
                value: None,
                description: None,
                bounds: None,
                state: UnifiedState::default(),
                is_interactive: false,
                level: None,
                automation_id: None,
                class_name: None,
                html_tag: None,
                url: None,
                source: NodeSource::Uia,
                platform_handle: None,
                supported_patterns: vec![],
                generation: 0,
                children: vec![],
            }
        }
    }

    #[test]
    fn test_by_role() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .by_role(UnifiedRole::Button)
            .find_all(&tree);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name.as_deref(), Some("Submit Form"));
    }

    #[test]
    fn test_by_label() {
        let tree = make_tree();
        let results = QueryBuilder::new().by_label("Email").find_all(&tree);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].role, UnifiedRole::Textbox);
    }

    #[test]
    fn test_by_label_contains() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .by_label_contains("Submit")
            .find_all(&tree);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ref_id, "@e1");
    }

    #[test]
    fn test_case_insensitive() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .by_label("email")
            .case_insensitive()
            .find_all(&tree);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_by_automation_id() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .by_automation_id("btn_submit")
            .find_all(&tree);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_interactive_only() {
        let tree = make_tree();
        let results = QueryBuilder::new().interactive().find_all(&tree);
        assert_eq!(results.len(), 3); // Button, Textbox, Checkbox
    }

    #[test]
    fn test_combined_filters() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .by_role(UnifiedRole::Textbox)
            .interactive()
            .find_all(&tree);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name.as_deref(), Some("Email"));
    }

    #[test]
    fn test_by_state() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .by_state(StateMatcher {
                checked: Some(TriBool::True),
                ..Default::default()
            })
            .find_all(&tree);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name.as_deref(), Some("Remember me"));
    }

    #[test]
    fn test_by_value() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .by_value("user@example.com")
            .find_all(&tree);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_limit() {
        let tree = make_tree();
        let results = QueryBuilder::new().limit(2).find_all(&tree);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_find_first() {
        let tree = make_tree();
        let result = QueryBuilder::new().interactive().find_first(&tree);
        assert!(result.is_some());
        assert_eq!(result.unwrap().ref_id, "@e1");
    }

    #[test]
    fn test_not_filter() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .filter(By::Not(Box::new(By::Interactive)))
            .find_all(&tree);
        // Window + StaticText = 2 non-interactive
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_or_filter() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .filter(By::Or(vec![
                By::Role(UnifiedRole::Button),
                By::Role(UnifiedRole::Checkbox),
            ]))
            .find_all(&tree);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_no_matches() {
        let tree = make_tree();
        let results = QueryBuilder::new()
            .by_label("NonexistentLabel")
            .find_all(&tree);
        assert!(results.is_empty());
    }

    #[test]
    fn test_empty_tree() {
        let empty = UnifiedNode {
            ref_id: "@e0".into(),
            role: UnifiedRole::Window,
            ..Default::default()
        };
        let results = QueryBuilder::new().interactive().find_all(&empty);
        assert!(results.is_empty());
    }
}
