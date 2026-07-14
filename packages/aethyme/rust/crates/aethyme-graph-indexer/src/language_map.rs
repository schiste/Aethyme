//! File classification: language detection from extension, code vs
//! non-code distinction.
//!
//! The mapping is intentionally HAND-MAINTAINED rather than
//! derived from a library. Per the schema's "no schema versioning"
//! discipline, the canonical language names ("python", "rust",
//! "typescript", etc.) appear in node IDs and on the wire; their
//! values are forever. Reflecting them from a third-party crate
//! would tie our forever-format to that crate's stability.

use aethyme_graph_schema::NonCodeFormat;

/// What kind of node a source file should produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileClassification {
    /// A code file. Carries its canonical language tag (used as the
    /// `language` field on the resulting `File` node).
    Code { language: &'static str },
    /// A non-code file with a recognized format (markdown, yaml,
    /// json, toml). Produces a `NonCodeFile` node with the typed
    /// format variant.
    NonCode { format: NonCodeFormat },
    /// A file the indexer should ignore entirely: binary blobs,
    /// build artifacts, generated lockfiles. The filesystem walker
    /// skips these.
    Ignored,
}

/// Classify a file by its extension. Returns `Ignored` for
/// extensions not in the recognized set — caller should skip those.
///
/// The match is case-insensitive on the extension (`README.MD` vs
/// `README.md` are both Markdown). Files with no extension are
/// considered Ignored unless the basename explicitly matches a
/// recognized pattern (currently none; reserved for future
/// `Makefile`, `Dockerfile`, etc. support).
pub fn classify_file(file_name: &str) -> FileClassification {
    let lower = file_name.to_ascii_lowercase();

    // Recognize bare-name (no-extension) source files.
    match lower.as_str() {
        "project.godot" => {
            return FileClassification::NonCode {
                format: NonCodeFormat::Other("godot".into()),
            };
        }
        "dockerfile" => {
            return FileClassification::Code {
                language: "dockerfile",
            };
        }
        "makefile" => {
            return FileClassification::Code {
                language: "makefile",
            };
        }
        _ => {}
    }

    let Some(ext) = extension(&lower) else {
        return FileClassification::Ignored;
    };

    if let Some(lang) = infer_language_from_extension(ext) {
        return FileClassification::Code { language: lang };
    }

    if let Some(format) = non_code_format_from_extension(ext) {
        return FileClassification::NonCode { format };
    }

    FileClassification::Ignored
}

/// Canonical language tag for a file extension, or None if not
/// a recognized code extension.
pub fn infer_language_from_extension(extension: &str) -> Option<&'static str> {
    // Map of extension (lowercase, no leading dot) → canonical
    // language tag. Tag values are forever per the schema's
    // "no versioning" rule, so canonical English snake_case.
    match extension {
        // Mainstream languages (alphabetical for grep-ability)
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some("cpp"),
        "cs" => Some("csharp"),
        "ex" | "exs" => Some("elixir"),
        "go" => Some("go"),
        "hs" => Some("haskell"),
        "java" => Some("java"),
        "js" | "cjs" | "mjs" | "jsx" => Some("javascript"),
        "kt" | "kts" => Some("kotlin"),
        "lua" => Some("lua"),
        "php" => Some("php"),
        "py" | "pyi" => Some("python"),
        "rb" => Some("ruby"),
        "rs" => Some("rust"),
        "scala" | "sc" => Some("scala"),
        "swift" => Some("swift"),
        "ts" | "tsx" => Some("typescript"),
        // Shell / scripting
        "sh" | "bash" | "zsh" => Some("shell"),
        _ => None,
    }
}

/// Non-code format for a file extension, or None.
fn non_code_format_from_extension(extension: &str) -> Option<NonCodeFormat> {
    match extension {
        "md" | "markdown" => Some(NonCodeFormat::Markdown),
        "yaml" | "yml" => Some(NonCodeFormat::Yaml),
        "json" => Some(NonCodeFormat::Json),
        "toml" => Some(NonCodeFormat::Toml),
        "txt" => Some(NonCodeFormat::Plain),
        // The Other(...) escape hatch for recognized-but-not-typed
        // text formats. Adding more typed variants is a NonCodeFormat
        // contract change.
        "xml" | "html" | "css" | "scss" | "rst" | "tex" | "ini" | "cfg" => {
            Some(NonCodeFormat::Other(extension.into()))
        }
        _ => None,
    }
}

fn extension(lower_file_name: &str) -> Option<&str> {
    lower_file_name.rsplit_once('.').map(|(_, ext)| ext)
}
