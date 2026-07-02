//! The only language-specific surface of the LSP client: a map from a file's
//! language (resolved by extension, mirroring `Syntax::for_path`) to the
//! command that launches its server. Built-in defaults, overridable by a user
//! config file. Adding a language is data, not code.

use std::path::Path;

use serde::Deserialize;

/// A language server launch command.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerCmd {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl ServerCmd {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// The language key for a path, by extension — the same dispatch shape the
/// syntax highlighter uses. `None` for extensions we don't map.
pub fn language_of(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        _ => return None,
    })
}

/// The built-in default server command for a language, if one is known.
fn default_command(language: &str) -> Option<ServerCmd> {
    Some(match language {
        "rust" => ServerCmd::new("rust-analyzer", &[]),
        "python" => ServerCmd::new("pyright-langserver", &["--stdio"]),
        "typescript" | "javascript" => ServerCmd::new("typescript-language-server", &["--stdio"]),
        "go" => ServerCmd::new("gopls", &[]),
        "c" | "cpp" => ServerCmd::new("clangd", &[]),
        _ => return None,
    })
}

/// The registry: built-in defaults plus user overrides, resolving a language to
/// the command that should serve it.
#[derive(Debug, Default)]
pub struct Registry {
    /// User-configured overrides, keyed by language; these win over defaults.
    overrides: std::collections::HashMap<String, ServerCmd>,
}

impl Registry {
    /// Load user overrides from `~/.config/vybim/lsp.json` if present. The file
    /// is a JSON object mapping a language to `{ "program": ..., "args": [...] }`.
    /// (JSON rather than TOML to avoid a new dependency; the path is otherwise
    /// as the design describes.) A missing or malformed file yields defaults.
    pub fn load() -> Self {
        let mut reg = Registry::default();
        if let Some(path) = config_path()
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(map) =
                serde_json::from_str::<std::collections::HashMap<String, ServerCmd>>(&text)
        {
            reg.overrides = map;
        }
        reg
    }

    /// Resolve the command to serve `language`: a user override beats the
    /// default. `None` when the language is unmapped, or the resolved program
    /// is not found on `PATH` (so a missing server is a silent no-op).
    pub fn resolve(&self, language: &str) -> Option<ServerCmd> {
        let cmd = self
            .overrides
            .get(language)
            .cloned()
            .or_else(|| default_command(language))?;
        if program_on_path(&cmd.program) {
            Some(cmd)
        } else {
            None
        }
    }

    #[cfg(test)]
    fn with_override(mut self, language: &str, cmd: ServerCmd) -> Self {
        self.overrides.insert(language.to_string(), cmd);
        self
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join(".config/vybim/lsp.json"))
}

/// Whether `program` can be found: an absolute/relative path that exists, or a
/// bare name present in some `PATH` directory.
fn program_on_path(program: &str) -> bool {
    let p = Path::new(program);
    if p.components().count() > 1 {
        return p.exists();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_resolves_by_extension() {
        assert_eq!(language_of(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(language_of(Path::new("a.py")), Some("python"));
        assert_eq!(language_of(Path::new("a.tsx")), Some("typescript"));
        assert_eq!(language_of(Path::new("README")), None);
        assert_eq!(language_of(Path::new("a.unknownext")), None);
    }

    #[test]
    fn default_resolution_requires_program_on_path() {
        // Use a program we know is present in a POSIX PATH: `sh`.
        let reg = Registry::default().with_override("toy", ServerCmd::new("sh", &["-c", "true"]));
        assert_eq!(
            reg.resolve("toy"),
            Some(ServerCmd::new("sh", &["-c", "true"]))
        );
    }

    #[test]
    fn user_override_beats_default() {
        let reg =
            Registry::default().with_override("rust", ServerCmd::new("sh", &["--my-analyzer"]));
        // Override present + `sh` on PATH → override wins over rust-analyzer.
        assert_eq!(
            reg.resolve("rust"),
            Some(ServerCmd::new("sh", &["--my-analyzer"]))
        );
    }

    #[test]
    fn unknown_language_resolves_to_none() {
        let reg = Registry::default();
        assert_eq!(reg.resolve("cobol"), None);
    }

    #[test]
    fn missing_program_resolves_to_none() {
        let reg = Registry::default().with_override(
            "toy",
            ServerCmd::new("definitely-not-a-real-program-xyz", &[]),
        );
        assert_eq!(reg.resolve("toy"), None);
    }
}
