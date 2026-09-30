use std::path::Path;

use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};

/// Registers the grammars the editor doesn't ship with.
pub fn register() {
    LanguageRegistry::singleton().register(
        "xml",
        &LanguageConfig::new(
            "xml",
            tree_sitter_xml::LANGUAGE_XML.into(),
            vec![],
            tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            "",
            "",
        ),
    );
}

/// The viewer's language name (tree-sitter grammar) for a path.
pub fn for_path(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match name {
        "Cargo.lock" | "Pipfile" => return "toml",
        "Makefile" | "makefile" | "GNUmakefile" => return "make",
        ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" | ".zprofile" => return "bash",
        _ => {}
    }
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "go" => "go",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "sql" => "sql",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "sh" | "bash" | "zsh" => "bash",
        "xml" | "xsd" | "xsl" | "xslt" | "svg" | "plist" | "csproj" | "props" => "xml",
        "py" | "pyi" => "python",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "mk" => "make",
        _ => "text",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn xml_grammar_and_query_match() {
        let language: tree_sitter::Language = tree_sitter_xml::LANGUAGE_XML.into();
        tree_sitter::Query::new(&language, tree_sitter_xml::XML_HIGHLIGHT_QUERY).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(r#"<model id="1"><field name="a"/></model>"#, None).unwrap();
        assert!(!tree.root_node().has_error());
    }
}
