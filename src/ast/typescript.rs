//! TypeScript and TSX adapter: classes, interfaces, enums, type aliases, namespaces, functions and
//! class members. `export` wrappers are transparent; a `variable_declarator` is a member only when
//! its initializer is a function or arrow function.

use super::{Language, MemberRule, MemberSpec};

/// The TypeScript language entry (`.ts`, `.mts`, `.cts`).
pub const TYPESCRIPT: Language = Language {
    id: "typescript",
    extensions: &["ts", "mts", "cts"],
    grammar: grammar_typescript,
    separator: ".",
    members: TYPESCRIPT_MEMBERS,
};

/// The TSX language entry, sharing the TypeScript node table.
pub const TSX: Language = Language {
    id: "tsx",
    extensions: &["tsx"],
    grammar: grammar_tsx,
    separator: ".",
    members: TYPESCRIPT_MEMBERS,
};

/// Node kinds that become annotatable members, in match order.
pub const TYPESCRIPT_MEMBERS: &[MemberSpec] = &[
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
        node_kind: "abstract_class_declaration",
        member_kind: "class",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "interface_declaration",
        member_kind: "interface",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: Some("object_type"),
    },
    MemberSpec {
        node_kind: "internal_module",
        member_kind: "module",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "enum_declaration",
        member_kind: "enum",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: Some("enum_body"),
    },
    MemberSpec {
        node_kind: "type_alias_declaration",
        member_kind: "type",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
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
        node_kind: "method_definition",
        member_kind: "method",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("statement_block"),
    },
    MemberSpec {
        node_kind: "abstract_method_signature",
        member_kind: "method",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "public_field_definition",
        member_kind: "field",
        name_field: Some("name"),
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

fn grammar_typescript() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn grammar_tsx() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}
