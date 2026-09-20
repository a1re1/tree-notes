//! JavaScript adapter (also used for JSX files): classes, class members, functions and
//! function-valued `const`s. `export` wrappers are transparent.

use super::{Language, MemberRule, MemberSpec};

/// The JavaScript language entry.
pub const JAVASCRIPT: Language = Language {
    id: "javascript",
    extensions: &["js", "mjs", "cjs", "jsx"],
    grammar,
    separator: ".",
    members: JAVASCRIPT_MEMBERS,
};

/// Node kinds that become annotatable members, in match order.
pub const JAVASCRIPT_MEMBERS: &[MemberSpec] = &[
    MemberSpec {
        node_kind: "export_statement",
        member_kind: "module",
        name_field: None,
        rule: MemberRule::Transparent,
        container: false,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "class_declaration",
        member_kind: "class",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "function_declaration",
        member_kind: "function",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("statement_block"),
    },
    MemberSpec {
        node_kind: "function_expression",
        member_kind: "function",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("statement_block"),
    },
    MemberSpec {
        node_kind: "method_definition",
        member_kind: "method",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("statement_block"),
    },
    MemberSpec {
        node_kind: "field_definition",
        member_kind: "field",
        name_field: Some("property"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "variable_declarator",
        member_kind: "function",
        name_field: Some("name"),
        rule: MemberRule::FunctionValue,
        container: false,
        body_kind: None,
    },
];

fn grammar() -> tree_sitter::Language {
    tree_sitter_javascript::LANGUAGE.into()
}
