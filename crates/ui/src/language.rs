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
            &xml_query(),
            "",
            "",
        ),
    );
}

/// The XML grammar's highlights, colored as VS Code colors XML: the
/// attributes' names and values each their color, and `<`, `>` like the
/// tag's name, not like plain text.
fn xml_query() -> String {
    const CHANGES: [(&str, &str); 4] = [
        ("(Attribute (Name) @property)", "(Attribute (Name) @attribute)"),
        ("(Attribute (AttValue) @string)", "(Attribute (AttValue) @attribute.value)"),
        (" \"<\" \">\"\n \"</\" \"/>\"\n] @punctuation.delimiter", " \"<\" \">\"\n \"</\" \"/>\"\n] @tag.delimiter"),
        ("[ \"\\\"\" \"'\" ] @punctuation.delimiter", "[ \"\\\"\" \"'\" ] @attribute.value"),
    ];
    debug_assert!(CHANGES.iter().all(|(from, _)| tree_sitter_xml::XML_HIGHLIGHT_QUERY.contains(from)));
    CHANGES
        .iter()
        .fold(tree_sitter_xml::XML_HIGHLIGHT_QUERY.to_string(), |query, (from, to)| query.replace(from, to))
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
        let query = super::xml_query();
        tree_sitter::Query::new(&language, &query).unwrap();
        // Every change found what it changes (a new grammar may word it otherwise).
        for capture in ["(Attribute (Name) @attribute)", "(Attribute (AttValue) @attribute.value)", "] @tag.delimiter", "\"'\" ] @attribute.value"] {
            assert!(query.contains(capture), "{capture}");
        }
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(r#"<model id="1"><field name="a"/></model>"#, None).unwrap();
        assert!(!tree.root_node().has_error());
    }

    /// XML and JSON in VS Code's colors: a tag, its `<`, an attribute and
    /// its value; a key and a string value, each its own.
    #[test]
    fn xml_and_json_are_colored_as_vs_code() {
        use gpui_kit::component::highlighter::{HighlightTheme, SyntaxColors, SyntaxHighlighter};
        use gpui_kit::component::input::Rope;
        use gpui_kit::{Hsla, rgb};

        super::register();
        let colors: std::collections::HashMap<String, SyntaxColors> =
            serde_json::from_str(include_str!("../assets/themes/vscode-2026.json")).unwrap();
        let mut theme = (*HighlightTheme::default_light()).clone();
        theme.style.syntax = colors["light"].clone();
        let color = |language: &str, text: &str, needle: &str| -> Option<Hsla> {
            let mut highlighter = SyntaxHighlighter::new(language);
            highlighter.update(None, &Rope::from(text), None);
            let at = text.find(needle).unwrap();
            highlighter
                .styles(&(0..text.len()), &theme)
                .into_iter()
                .find(|(range, _)| range.contains(&at))
                .and_then(|(_, style)| style.color)
        };
        let xml = r#"<field name="exportA3"/>"#;
        assert_eq!(color("xml", xml, "field"), Some(rgb(0x800000).into()));
        assert_eq!(color("xml", xml, "<"), Some(rgb(0x800000).into()));
        assert_eq!(color("xml", xml, "name"), Some(rgb(0xe50000).into()));
        assert_eq!(color("xml", xml, "exportA3"), Some(rgb(0x0000ff).into()));
        let json = r#"{"key": "value", "list": ["item"], "n": 1}"#;
        assert_eq!(color("json", json, "key"), Some(rgb(0x0451a5).into()));
        assert_eq!(color("json", json, "value"), Some(rgb(0xa31515).into()));
        assert_eq!(color("json", json, "item"), Some(rgb(0xa31515).into()));
    }
}
