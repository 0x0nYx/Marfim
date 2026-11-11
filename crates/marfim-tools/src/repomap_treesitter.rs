//! W5.3 — tree-sitter symbol extraction (feature `treesitter`, default
//! OFF; DISTILLATION §FORMER-REJECTS adoption form). AST-accurate
//! definition extraction for py/rs/go/js/ts. Languages without a
//! compiled-in grammar (java/c/rb) and any file whose parse errors keep
//! it from yielding an AST fall back to the regex extractor — honestly,
//! per file, never silently guessed.

use std::path::Path;

use super::repomap::{lang_of, Symbol};

/// Extract symbols with the tree-sitter grammar for the file's language.
/// `Ok(symbols)` on a parse; `Err(ExtractMiss)` when no grammar covers the
/// language or the parse produced no usable tree (caller falls back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractMiss {
    /// Why the AST path refused — surfaced in tests and diagnostics only.
    pub reason: &'static str,
}

pub fn extract_treesitter(path: &Path, text: &str) -> Result<Vec<Symbol>, ExtractMiss> {
    let Some(lang) = lang_of(path) else {
        return Err(ExtractMiss {
            reason: "no language for extension",
        });
    };
    let mut parser = tree_sitter::Parser::new();
    // ts/tsx need the TYPESCRIPT grammar (the JS grammar does not model
    // `interface` or `type` aliases); the `js` lang tag alone is too
    // coarse, so the extension decides. .tsx rides LANGUAGE_TSX (JSX
    // elements in types).
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let language = match lang {
        "py" => tree_sitter_python::LANGUAGE.into(),
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "js" if ext == "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" if ["ts", "mts", "cts"].contains(&ext.as_str()) => {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
        "js" => tree_sitter_javascript::LANGUAGE.into(),
        _ => {
            return Err(ExtractMiss {
                reason: "no grammar compiled in",
            })
        }
    };
    parser.set_language(&language).map_err(|_| ExtractMiss {
        reason: "grammar version mismatch",
    })?;
    let tree = parser.parse(text, None).ok_or(ExtractMiss {
        reason: "parser allocation failed",
    })?;
    let root = tree.root_node();

    let mut out = Vec::new();
    walk(&root, text.as_bytes(), &mut out);
    Ok(out)
}

/// Depth-first walk collecting def nodes. Node kinds are the canonical
/// grammar names; every name is read off the AST (identifier/property
/// identifier field or first named child), never re-parsed from text.
fn walk(node: &tree_sitter::Node, src: &[u8], out: &mut Vec<Symbol>) {
    let kind = node.kind();
    match kind {
        // python
        "function_definition" | "class_definition" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(
                    out,
                    name,
                    if kind == "class_definition" {
                        "class"
                    } else {
                        "fn"
                    },
                    node,
                );
            }
        }
        // rust
        "function_item" | "function_signature_item" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(out, name, "fn", node);
            }
        }
        "struct_item" | "enum_item" | "union_item" | "type_item" => {
            if let Some(name) = child_field_text(node, "name", src) {
                let k = match kind {
                    "struct_item" => "struct",
                    "enum_item" => "enum",
                    "type_item" => "type",
                    _ => "struct",
                };
                push(out, name, k, node);
            }
        }
        "trait_item" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(out, name, "trait", node);
            }
        }
        "impl_item" => {
            if let Some(ty) = child_field_text(node, "type", src) {
                push(out, ty, "impl", node);
            }
        }
        "mod_item" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(out, name, "mod", node);
            }
        }
        // go
        "function_declaration" | "method_declaration" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(out, name, "fn", node);
            }
        }
        "type_declaration" => {
            // each `type X <spec>` is a child spec
            collect_go_type_specs(node, src, out);
        }
        // js/ts — `function_declaration` was already taken by the go arm
        // above (both want "fn"); classes and interfaces live here.
        "class_declaration" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(out, name, "class", node);
            }
        }
        "method_definition" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(out, name, "fn", node);
            }
        }
        "interface_declaration" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(out, name, "class", node);
            }
        }
        "type_alias_declaration" => {
            if let Some(name) = child_field_text(node, "name", src) {
                push(out, name, "type", node);
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // `export const LIMIT = 3` — collect every declarator name
            // (superset of the regex extractor's first-match shape).
            collect_js_const_names(node, src, out);
        }
        _ => {}
    }
    let mut child_cursor = node.walk();
    for child in node.children(&mut child_cursor) {
        walk(&child, src, out);
    }
}

fn collect_go_type_specs(node: &tree_sitter::Node, src: &[u8], out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "type_spec" || child.kind() == "type_spec_alias" {
            if let Some(name) = child_field_text(&child, "name", src) {
                let is_iface = child
                    .child_by_field_name("type")
                    .map(|t| t.kind() == "interface_type")
                    .unwrap_or(false);
                push(out, name, if is_iface { "class" } else { "struct" }, &child);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn collect_js_const_names(node: &tree_sitter::Node, src: &[u8], out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "variable_declarator" {
            if let Some(name) = child_field_text(&child, "name", src) {
                push(out, name, "def", &child);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn child_field_text(node: &tree_sitter::Node, field: &str, src: &[u8]) -> Option<String> {
    let n = node.child_by_field_name(field)?;
    let text = n.utf8_text(src).ok()?;
    Some(text.to_string())
}

fn push(out: &mut Vec<Symbol>, name: String, kind: &'static str, node: &tree_sitter::Node) {
    if name.is_empty() || name.len() >= 128 {
        return;
    }
    out.push(Symbol {
        name,
        kind,
        line: node.start_position().row + 1,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_defs_via_ast() {
        let src = "pub async fn worker() {}\n\
                   pub(crate) fn helper() {}\n\
                   struct Inner { x: u8 }\n\
                   enum Choice { A }\n\
                   trait Raw { }\n\
                   impl Inner {}\n\
                   mod sub {}\n";
        let syms = extract_treesitter(Path::new("src/a.rs"), src).expect("parse");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        for expected in ["worker", "helper", "Inner", "Choice", "Raw", "sub"] {
            assert!(names.contains(&expected), "{expected} missing: {names:?}");
        }
        // impl names the TYPE it implements
        assert!(syms.iter().any(|s| s.name == "Inner" && s.kind == "impl"));
    }

    #[test]
    fn python_and_go_defs_via_ast() {
        let py = extract_treesitter(
            Path::new("m.py"),
            "async def fetch(x):\n    return x\n\nclass Two:\n    pass\n",
        )
        .expect("parse");
        assert!(py.iter().any(|s| s.name == "fetch" && s.kind == "fn"));
        assert!(py.iter().any(|s| s.name == "Two" && s.kind == "class"));

        let go = extract_treesitter(
            Path::new("svc.go"),
            "func (s *Server) Start() error { return nil }\nfunc Plain() {}\ntype Config struct { A int }\ntype Store interface { Get() }\n",
        )
        .expect("parse");
        let names: Vec<&str> = go.iter().map(|s| s.name.as_str()).collect();
        for expected in ["Start", "Plain", "Config", "Store"] {
            assert!(names.contains(&expected), "{expected} missing: {names:?}");
        }
    }

    #[test]
    fn ts_defs_via_ast() {
        let ts = extract_treesitter(
            Path::new("types.ts"),
            "export interface Options { a: number }\n\
             export type Alias = string;\n\
             export async function run(): Promise<void> {}\n\
             export default class Main {}\n",
        )
        .expect("parse");
        let names: Vec<&str> = ts.iter().map(|s| s.name.as_str()).collect();
        for expected in ["Options", "Alias", "run", "Main"] {
            assert!(names.contains(&expected), "{expected} missing: {names:?}");
        }
    }

    #[test]
    fn languages_without_grammar_fall_back() {
        // java/c/rb have no compiled-in grammar → honest Err, the caller
        // (repomap::extract) falls back to the regex extractor.
        assert!(extract_treesitter(Path::new("A.java"), "class A {}").is_err());
        assert!(extract_treesitter(Path::new("a.c"), "int main() {}").is_err());
    }

    #[test]
    fn def_lines_match_the_ast_not_the_text() {
        // A commented-out def in python is a STRING node, not a
        // function_definition — the regex extractor's line scan would
        // never see it either, but a same-line `def ghost(): pass` inside
        // a docstring proves the AST path (the regex path WOULD see it if
        // it were not comment-prefixed).
        let src = "def real(): pass\nDOC = \"\"\"\ndef ghost(): pass\n\"\"\"\n";
        let syms = extract_treesitter(Path::new("m.py"), src).expect("parse");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"real"));
        assert!(
            !names.contains(&"ghost"),
            "docstring defs must not count: {names:?}"
        );
    }

    #[test]
    fn lines_are_one_based() {
        let src = "\n\nfn later() {}\n";
        let syms = extract_treesitter(Path::new("a.rs"), src).expect("parse");
        assert_eq!(syms[0].name, "later");
        assert_eq!(syms[0].line, 3);
    }
}
