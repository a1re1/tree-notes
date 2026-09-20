//! AST-backed members: parse one supported source file and list its annotatable declarations.
//!
//! A member note is bound to content exactly like a file note, but the content is the declaration
//! itself: the symbol key (`<kind>:<qualified name>:<ordinal>`) plus the declaration's normalised
//! source text, hashed with the `tnt2` member scheme (see [`hash`]). Editing one method therefore
//! re-stales only that method's note; siblings stay fresh.
//!
//! The language registry is data: each adapter is a table of node kinds, so a new language is a new
//! module plus one [`Language`] entry, with no change to hashing, storage, CLI or output.

use std::collections::BTreeMap;
use std::path::Path;

use tree_sitter::{Node, Parser};

use crate::CmdError;

pub mod hash;
pub mod java;
pub mod javascript;
pub mod python;
pub mod rust;
pub mod typescript;

/// How a matched node becomes (or does not become) a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberRule {
    /// The node itself is the member.
    Plain,
    /// The node is transparent: its children are visited in the same container, one level deeper.
    Transparent,
    /// The node is the member, but its kind, name and children come from its `definition` child
    /// (Python `decorated_definition`: the decorators are part of the hashed body).
    Decorated,
    /// The node is a member only when it binds a function (see [`Spec`]).
    FunctionValue,
}

/// One node kind that becomes an annotatable member.
#[derive(Clone, Copy, Debug)]
pub struct MemberSpec {
    /// tree-sitter node kind, e.g. `method_declaration`.
    pub node_kind: &'static str,
    /// treenotes member kind: `method`, `function`, `class`, ...
    pub member_kind: &'static str,
    /// Field holding the name (`declarator.name` descends one field further); `None` falls back to
    /// the first identifier-like child, then to `<anonymous>` plus the ordinal.
    pub name_field: Option<&'static str>,
    /// How the matched node becomes a member.
    pub rule: MemberRule,
    /// When true the node is a container: its children (except `body_kind`) are visited with this
    /// member's name prepended to their qualified names.
    pub container: bool,
    /// Node kind whose subtree is skipped inside a container (e.g. `enum_body`), so container
    /// members do not silently become annotatable members themselves.
    pub body_kind: Option<&'static str>,
}

/// One language treenotes can parse.
#[derive(Clone, Copy, Debug)]
pub struct Language {
    /// Stable language id: `java`, `rust`, `typescript`, `tsx`, `javascript`, `python`.
    pub id: &'static str,
    /// File extensions that select this language (lowercase, without the dot).
    pub extensions: &'static [&'static str],
    /// tree-sitter grammar handle.
    pub grammar: fn() -> tree_sitter::Language,
    /// Separator used to join a container name with the names inside it.
    pub separator: &'static str,
    /// Node kinds that become annotatable members.
    pub members: &'static [MemberSpec],
}

/// Every language this build can parse.
pub fn registry() -> &'static [Language] {
    static LANGUAGES: &[Language] = &[
        typescript::TYPESCRIPT,
        typescript::TSX,
        javascript::JAVASCRIPT,
        java::JAVA,
        rust::RUST,
        python::PYTHON,
    ];
    LANGUAGES
}

/// The language for a repository-root relative path, chosen by its extension.
pub fn language_for_path(path: &str) -> Option<&'static Language> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    let extension = extension.to_ascii_lowercase();
    registry()
        .iter()
        .find(|language| language.extensions.contains(&extension.as_str()))
}

/// One annotatable declaration of a parsed file.
#[derive(Clone, Debug)]
pub struct Member {
    /// Symbol key, `<symbol-kind>:<qualified name>:<ordinal>`.
    pub symbol: String,
    /// treenotes member kind.
    pub symbol_kind: String,
    /// Declared name as written, unqualified.
    pub name: String,
    /// Name qualified by its container chain (`Cache.put`, `Foo::bar`, `outer.inner`).
    pub qualified_name: String,
    /// 1-based line of the declaration's first line (display metadata, never part of identity).
    pub start_line: usize,
    /// 1-based line of the declaration's last line (display metadata).
    pub end_line: usize,
    /// `tnt2:member:<hex>` hash of this declaration.
    pub hash: String,
}

/// The `tnt2:member:` scheme tag, so callers can recognise a member hash without duplicating the
/// constant.
pub fn member_scheme() -> &'static str {
    hash::MEMBER_SCHEME
}

/// Members of one file, plus whether the grammar had to recover from a syntax error.
#[derive(Clone, Debug)]
pub struct ParseResult {
    /// Members in document order.
    pub members: Vec<Member>,
    /// True when the parse tree contains an error (members are still listed from the recovered
    /// tree).
    pub parse_error: bool,
}

/// Read a source file strictly as UTF-8; a file that cannot be read or decoded is an environment
/// failure, never a silent gap.
pub fn read_source(absolute: &Path, label: &str) -> Result<String, CmdError> {
    let bytes =
        std::fs::read(absolute).map_err(|e| CmdError::env(format!("cannot read {label}: {e}")))?;
    String::from_utf8(bytes).map_err(|_| {
        CmdError::env(format!(
            "{label} is not valid UTF-8; treenotes refuses to drop it"
        ))
    })
}

/// Parse `source` with `language` and list its members in document order.
pub fn parse_members(language: &Language, source: &str) -> Result<ParseResult, CmdError> {
    let grammar = (language.grammar)();
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|e| CmdError::env(format!("cannot load the {} grammar: {e}", language.id)))?;
    let tree = parser.parse(source, None).ok_or_else(|| {
        CmdError::env(format!(
            "cannot parse this file with the {} grammar",
            language.id
        ))
    })?;
    let root = tree.root_node();
    let mut collector = Collector {
        language,
        source: source.as_bytes(),
        members: Vec::new(),
        ordinals: BTreeMap::new(),
    };
    collector.visit(root, &[], false);
    Ok(ParseResult {
        members: collector.members,
        parse_error: root.has_error(),
    })
}

/// Depth-first member collector.
struct Collector<'a> {
    language: &'a Language,
    source: &'a [u8],
    members: Vec<Member>,
    /// Next ordinal per (member kind, qualified name) inside one container.
    ordinals: BTreeMap<(String, String), usize>,
}

impl Collector<'_> {
    fn spec_for(&self, node_kind: &str) -> Option<&'static MemberSpec> {
        self.language
            .members
            .iter()
            .find(|spec| spec.node_kind == node_kind)
    }

    fn visit(&mut self, node: Node, qualified_parents: &[String], inside_function: bool) {
        let spec = match self.spec_for(node.kind()) {
            Some(spec) => spec,
            None => {
                self.descend(node, qualified_parents, None, inside_function);
                return;
            }
        };
        match spec.rule {
            MemberRule::Transparent => self.descend(node, qualified_parents, None, inside_function),
            MemberRule::Decorated => {
                let inner = node
                    .child_by_field_name("definition")
                    .and_then(|inner| self.spec_for(inner.kind()).map(|spec| (inner, spec)));
                match inner {
                    Some((inner, inner_spec)) => {
                        let name = self
                            .resolve_name(inner, inner_spec)
                            .unwrap_or_else(|| "<anonymous>".to_string());
                        let qualified = qualify(qualified_parents, &name, self.language.separator);
                        self.record(node, inner_spec.member_kind, &name, &qualified);
                        let mut children = qualified_parents.to_vec();
                        children.push(name);
                        let nested = inside_function || inner_spec.member_kind == "function";
                        self.descend(inner, &children, inner_spec.body_kind, nested);
                    }
                    None => self.descend(node, qualified_parents, None, inside_function),
                }
            }
            // Constants are module/class-level only: a `TIMEOUT = 5` local inside a function body
            // is an implementation detail, not an addressable declaration.
            MemberRule::FunctionValue => {
                if !inside_function && self.binds_function(node) {
                    let name = self
                        .resolve_name(node, spec)
                        .unwrap_or_else(|| "<anonymous>".to_string());
                    let qualified = qualify(qualified_parents, &name, self.language.separator);
                    self.record(node, spec.member_kind, &name, &qualified);
                }
            }
            MemberRule::Plain => {
                let name = self
                    .resolve_name(node, spec)
                    .unwrap_or_else(|| "<anonymous>".to_string());
                let qualified = qualify(qualified_parents, &name, self.language.separator);
                self.record(node, spec.member_kind, &name, &qualified);
                if spec.container {
                    let mut children = qualified_parents.to_vec();
                    children.push(name);
                    let nested = inside_function || spec.member_kind == "function";
                    self.descend(node, &children, spec.body_kind, nested);
                }
            }
        }
    }

    /// Visit the named children of `node`, skipping `body_kind` and recording nothing itself.
    fn descend(
        &mut self,
        node: Node,
        qualified_parents: &[String],
        body_kind: Option<&str>,
        inside_function: bool,
    ) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if body_kind == Some(child.kind()) {
                continue;
            }
            self.visit(child, qualified_parents, inside_function);
        }
    }

    /// The declared name: the configured field path, else the first identifier-like child.
    fn resolve_name(&self, node: Node, spec: &MemberSpec) -> Option<String> {
        if let Some(path) = spec.name_field {
            let mut current = node;
            let mut found = true;
            for step in path.split('.') {
                match current.child_by_field_name(step) {
                    Some(child) => current = child,
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            if found {
                if let Ok(text) = current.utf8_text(self.source) {
                    let text = text.trim();
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(
                child.kind(),
                "identifier" | "type_identifier" | "property_identifier" | "field_identifier"
            ) {
                if let Ok(text) = child.utf8_text(self.source) {
                    let text = text.trim();
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
        }
        None
    }

    /// True when a [`MemberRule::FunctionValue`] node binds a function: a TypeScript/JavaScript
    /// `variable_declarator` whose initializer is a function, or a Python `NAME = ...`/`x = lambda`
    /// assignment.
    fn binds_function(&self, node: Node) -> bool {
        if let Some(value) = node.child_by_field_name("value") {
            if matches!(
                value.kind(),
                "arrow_function" | "function" | "function_expression" | "lambda"
            ) {
                return true;
            }
        }
        if let Some(left) = node.child_by_field_name("left") {
            if left.kind() == "identifier" {
                if let Ok(name) = left.utf8_text(self.source) {
                    if is_screaming_case(name) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Record one member: assign its ordinal, build its symbol key and hash its own text.
    fn record(&mut self, node: Node, member_kind: &str, name: &str, qualified_name: &str) {
        let symbol_kind = if member_kind == "method" && name == "constructor" {
            "constructor"
        } else {
            member_kind
        };
        let ordinal = {
            let next = self
                .ordinals
                .entry((symbol_kind.to_string(), qualified_name.to_string()))
                .or_insert(0);
            let ordinal = *next;
            *next += 1;
            ordinal
        };
        let symbol = format!("{symbol_kind}:{qualified_name}:{ordinal}");
        let body = node.utf8_text(self.source).unwrap_or_default();
        self.members.push(Member {
            hash: hash::symbol_hash(&symbol, body),
            symbol,
            symbol_kind: symbol_kind.to_string(),
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
        });
    }
}

/// Join a container chain and a display name with the language separator.
fn qualify(parents: &[String], name: &str, separator: &str) -> String {
    if parents.is_empty() {
        name.to_string()
    } else {
        format!("{}{separator}{name}", parents.join(separator))
    }
}

/// Python constant rule: at least one letter, and every character uppercase, digit or underscore.
fn is_screaming_case(name: &str) -> bool {
    let mut has_letter = false;
    for character in name.chars() {
        if character.is_ascii_lowercase() {
            return false;
        }
        if character.is_ascii_uppercase() {
            has_letter = true;
        } else if !character.is_ascii_digit() && character != '_' {
            return false;
        }
    }
    has_letter
}
