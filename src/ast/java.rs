//! Java adapter: classes, interfaces, enums, records, annotations, methods, constructors, fields
//! and enum constants. Anonymous classes are skipped: they have no stable name.

use super::{Language, MemberRule, MemberSpec};

/// The Java language entry.
pub const JAVA: Language = Language {
    id: "java",
    extensions: &["java"],
    grammar,
    separator: ".",
    members: JAVA_MEMBERS,
};

/// Node kinds that become annotatable members, in match order.
pub const JAVA_MEMBERS: &[MemberSpec] = &[
    MemberSpec {
        node_kind: "class_declaration",
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
        body_kind: None,
    },
    MemberSpec {
        node_kind: "enum_declaration",
        member_kind: "enum",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "record_declaration",
        member_kind: "record",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "annotation_type_declaration",
        member_kind: "annotation",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "method_declaration",
        member_kind: "method",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("block"),
    },
    MemberSpec {
        node_kind: "constructor_declaration",
        member_kind: "constructor",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("block"),
    },
    MemberSpec {
        node_kind: "field_declaration",
        member_kind: "field",
        name_field: Some("declarator.name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "enum_constant",
        member_kind: "constant",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: None,
    },
];

fn grammar() -> tree_sitter::Language {
    tree_sitter_java::LANGUAGE.into()
}
