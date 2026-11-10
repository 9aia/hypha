//! hypha-config-file — the file impl of [`ConfigSource`]: resolves
//! which config file to load, then dispatches parsing by extension to
//! the [`ConfigFormat`]s. Search order (P-015, the shape every org
//! repo copy-pasted):
//!
//! explicit path → `$<NAME>_CONFIG` → `<dir>/<name>.<ext>` per search
//! dir (project dir, then `~/.config/<name>/`). Within a directory,
//! formats are tried in preference order and each format's extensions
//! in declared order — YAML before TOML by convention, so
//! `<name>.yaml` beats `<name>.toml` on ties.
//!
//! An explicit path whose extension no format claims is an error; an
//! extensionless explicit path parses with the first (preferred)
//! format — the back-compat seam for `--config` pointing at a file
//! without a suffix.

use std::path::{Path, PathBuf};

use hypha_config::{ConfigFormat, ConfigSource, Discovery, expand_tilde};
use hypha_config_toml::TomlFormat;
use hypha_config_yaml::YamlFormat;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// File-backed config source — discovery + extension dispatch over a
/// set of [`ConfigFormat<T>`]s for one config model `T`.
///
/// [`ConfigFormat<T>`]: hypha_config::ConfigFormat
pub struct FileSource<T> {
    /// Where to search — app basename, env var, dirs.
    pub discovery: Discovery,
    /// Explicit `--config`-style path; wins over `discovery.env_var`.
    pub explicit: Option<String>,
    /// Syntax formats in preference order — earlier formats win
    /// candidate ties within a directory.
    pub formats: Vec<Box<dyn ConfigFormat<T>>>,
}

impl<T> FileSource<T> {
    /// A source over `discovery` with caller-chosen formats (any
    /// order — earlier formats win candidate ties).
    #[must_use]
    pub fn with_formats(discovery: Discovery, formats: Vec<Box<dyn ConfigFormat<T>>>) -> Self {
        Self {
            discovery,
            explicit: None,
            formats,
        }
    }

    /// Every candidate path in search order. `env` supplies env-var
    /// lookups so callers/tests never have to touch the process
    /// environment.
    pub fn candidates_with(&self, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
        if let Some(p) = self
            .explicit
            .clone()
            .or_else(|| self.env_path(env))
            .filter(|p| !p.is_empty())
        {
            return vec![PathBuf::from(p)];
        }
        let name = &self.discovery.name;
        let mut v = Vec::new();
        for dir in &self.discovery.dirs {
            let dir = expand_tilde(dir);
            for format in &self.formats {
                for ext in format.extensions() {
                    v.push(dir.join(format!("{name}.{ext}")));
                }
            }
        }
        v
    }

    /// The first existing candidate. An explicit/env path that doesn't
    /// exist is an error naming it — a named path that misses is a
    /// config error, not a silent skip; discovery candidates skip
    /// silently.
    ///
    /// # Errors
    /// Fails when the explicit path is missing or no candidate exists.
    pub fn locate_with(&self, env: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
        let explicit = self
            .explicit
            .clone()
            .or_else(|| self.env_path(env))
            .filter(|p| !p.is_empty());
        if let Some(p) = explicit {
            let path = PathBuf::from(p);
            return if path.is_file() {
                Ok(path)
            } else {
                Err(format!("config file not found: {}", path.display()))
            };
        }
        self.candidates_with(env)
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| self.not_found_error())
    }

    /// The resolved config path using the process environment.
    ///
    /// # Errors
    /// Same as [`locate_with`](Self::locate_with).
    pub fn locate(&self) -> Result<PathBuf, String> {
        self.locate_with(&|k| std::env::var(k).ok())
    }

    /// Load the resolved file as `T` using the process environment.
    ///
    /// # Errors
    /// Fails on a missing file or a read/parse error.
    pub fn load(&self) -> Result<T, String> {
        self.load_with(&|k| std::env::var(k).ok())
    }

    /// Load the resolved file as `T`; `env` supplies env-var lookups.
    ///
    /// # Errors
    /// Fails on a missing file or a read/parse error.
    pub fn load_with(&self, env: &dyn Fn(&str) -> Option<String>) -> Result<T, String> {
        let path = self.locate_with(env)?;
        self.load_path(&path)
    }

    /// Parse a specific file — extension dispatch; an extensionless
    /// path parses with the first (preferred) format.
    ///
    /// # Errors
    /// Fails on a read error, an unclaimed extension, or a parse error.
    pub fn load_path(&self, path: &Path) -> Result<T, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let format = self.format_for_path(path).ok_or_else(|| {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");
            format!("no config format claims '.{ext}': {}", path.display())
        })?;
        format.parse(&text)
    }

    /// The env-var path, if `discovery.env_var` is set and non-empty.
    fn env_path(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
        self.discovery
            .env_var
            .as_deref()
            .and_then(env)
            .filter(|v| !v.is_empty())
    }

    /// Format for `path`: extension match (same rule as
    /// `hypha_config::format_for`, inlined so the borrow ties to
    /// `self`); an
    /// extensionless path falls back to the first (preferred) format.
    fn format_for_path<'a>(&'a self, path: &Path) -> Option<&'a dyn ConfigFormat<T>> {
        match path.extension().and_then(|e| e.to_str()) {
            // extensionless explicit paths parse as the preferred format
            None => self.formats.first().map(|f| &**f),
            Some(ext) => self
                .formats
                .iter()
                .find(|f| f.extensions().contains(&ext))
                .map(|f| &**f),
        }
    }

    /// The "nothing found" error — names the app, claimed extensions,
    /// and the search steps actually configured.
    fn not_found_error(&self) -> String {
        let exts: Vec<&str> = self
            .formats
            .iter()
            .flat_map(|f| f.extensions().iter().copied())
            .collect();
        let mut searched: Vec<String> = Vec::new();
        if let Some(var) = &self.discovery.env_var {
            searched.push(format!("${var}"));
        }
        for dir in &self.discovery.dirs {
            searched.push(expand_tilde(dir).display().to_string());
        }
        searched.push("…".to_string());
        format!(
            "no {}.{{{}}} found (search: {})",
            self.discovery.name,
            exts.join(","),
            searched.join(", ")
        )
    }
}

impl<T> FileSource<T>
where
    T: DeserializeOwned + Serialize,
{
    /// The org-standard format pair — YAML preferred, TOML fallback.
    #[must_use]
    pub fn standard_formats() -> Vec<Box<dyn ConfigFormat<T>>> {
        vec![Box::new(YamlFormat), Box::new(TomlFormat)]
    }

    /// A source over `discovery` with the standard formats.
    #[must_use]
    pub fn new(discovery: Discovery) -> Self {
        Self::with_formats(discovery, Self::standard_formats())
    }

    /// A source for app `name` — [`Discovery::for_app`] conventions +
    /// the standard formats.
    #[must_use]
    pub fn for_app(name: impl Into<String>) -> Self {
        Self::new(Discovery::for_app(name))
    }
}

impl<T> ConfigSource<T> for FileSource<T> {
    fn load(&self) -> Result<T, String> {
        self.load_with(&|k| std::env::var(k).ok())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(default)]
    struct Cfg {
        name: String,
        port: u16,
    }

    fn source_in(dir: &Path) -> FileSource<Cfg> {
        FileSource::new(
            Discovery::bare("myapp")
                .dirs([dir.to_path_buf()])
                .env_var("MYAPP_CONFIG"),
        )
    }

    #[test]
    fn yaml_wins_over_toml_in_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("myapp.toml"), "name = \"toml\"\nport = 1\n").unwrap();
        std::fs::write(dir.path().join("myapp.yaml"), "name: yaml\nport: 2\n").unwrap();
        let src = source_in(dir.path());
        let cfg = src.load_with(&|_| None).unwrap();
        assert_eq!(
            cfg,
            Cfg {
                name: "yaml".into(),
                port: 2
            }
        );
        assert_eq!(
            src.locate_with(&|_| None).unwrap(),
            dir.path().join("myapp.yaml")
        );
    }

    #[test]
    fn yml_fills_in_when_yaml_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("myapp.yml"), "name: yml\n").unwrap();
        std::fs::write(dir.path().join("myapp.toml"), "name = \"toml\"\n").unwrap();
        let src = source_in(dir.path());
        let cfg = src.load_with(&|_| None).unwrap();
        assert_eq!(cfg.name, "yml");
    }

    #[test]
    fn toml_only_and_candidate_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("myapp.toml"), "name = \"toml\"\nport = 3\n").unwrap();
        let src = source_in(dir.path());
        let cfg = src.load_with(&|_| None).unwrap();
        assert_eq!(
            cfg,
            Cfg {
                name: "toml".into(),
                port: 3
            }
        );
        // candidate order: yaml, yml, toml within the dir
        let c = src.candidates_with(&|_| None);
        assert_eq!(
            c,
            vec![
                dir.path().join("myapp.yaml"),
                dir.path().join("myapp.yml"),
                dir.path().join("myapp.toml"),
            ]
        );
    }

    #[test]
    fn earlier_dir_wins() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("myapp.toml"), "name = \"first\"\n").unwrap();
        std::fs::write(b.path().join("myapp.yaml"), "name: second\n").unwrap();
        // dir order beats format preference: first dir's toml wins
        let src = FileSource::<Cfg>::new(
            Discovery::bare("myapp").dirs([a.path().to_path_buf(), b.path().to_path_buf()]),
        );
        let cfg = src.load_with(&|_| None).unwrap();
        assert_eq!(cfg.name, "first");
    }

    #[test]
    fn explicit_path_and_missing_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("custom.toml");
        std::fs::write(&p, "name = \"explicit\"\n").unwrap();
        let mut src = source_in(dir.path());
        src.explicit = Some(p.to_string_lossy().into_owned());
        let cfg = src.load_with(&|_| None).unwrap();
        assert_eq!(cfg.name, "explicit");

        src.explicit = Some(dir.path().join("gone.yaml").to_string_lossy().into_owned());
        let err = src.load_with(&|_| None).unwrap_err();
        assert!(err.contains("config file not found"), "{err}");
    }

    #[test]
    fn env_var_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("env.toml");
        std::fs::write(&p, "name = \"env\"\n").unwrap();
        let src = source_in(dir.path());
        let env = |k: &str| (k == "MYAPP_CONFIG").then(|| p.to_string_lossy().into_owned());
        let cfg = src.load_with(&env).unwrap();
        assert_eq!(cfg.name, "env");
        // explicit beats env
        let mut src2 = source_in(dir.path());
        src2.explicit = Some(p.to_string_lossy().into_owned());
        assert_eq!(src2.locate_with(&env).unwrap(), p);
    }

    #[test]
    fn extensionless_explicit_uses_preferred_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cfg-noext");
        std::fs::write(&p, "name: bare-yaml\n").unwrap();
        let mut src = source_in(dir.path());
        src.explicit = Some(p.to_string_lossy().into_owned());
        let cfg = src.load_with(&|_| None).unwrap();
        assert_eq!(cfg.name, "bare-yaml");
    }

    #[test]
    fn unknown_extension_and_missing_everything() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cfg.json");
        std::fs::write(&p, "{}").unwrap();
        let mut src = source_in(dir.path());
        src.explicit = Some(p.to_string_lossy().into_owned());
        let err = src.load_with(&|_| None).unwrap_err();
        assert!(err.contains("no config format claims"), "{err}");

        let empty = tempfile::tempdir().unwrap();
        let src = source_in(empty.path());
        let err = src.load_with(&|_| None).unwrap_err();
        assert!(err.contains("no myapp."), "{err}");
        assert!(err.contains("MYAPP_CONFIG"), "{err}");
    }

    #[test]
    fn config_source_trait_impl() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("myapp.yaml"), "name: trait\n").unwrap();
        let src = source_in(dir.path());
        let cfg = ConfigSource::<Cfg>::load(&src).unwrap();
        assert_eq!(cfg.name, "trait");
    }
}
