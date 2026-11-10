//! hypha-config — the config port: where a typed config value comes
//! from, and how file bytes become one. Generic over the config
//! **model** — hypha never names a repo's `Config` type; each repo
//! brings its own serde model and picks sources + formats (P-015).
//!
//! Two layers:
//!
//! - [`ConfigFormat`] — syntax-level: which extensions a format
//!   claims, `parse`/`render` over serde. Impls: `hypha-config-toml`,
//!   `hypha-config-yaml`.
//! - [`ConfigSource<T>`] — source-level: `load() -> Result<T, String>`.
//!   Impls: `hypha-config-file` (discovery + extension dispatch);
//!   env/remote sources are new impls.
//!
//! [`Discovery`] is the shared search spec every file-backed source
//! uses: explicit path → `$<NAME>_CONFIG` → project dir → user config
//! dir, earlier formats winning ties (YAML preferred by convention).

use std::path::{Path, PathBuf};

/// A config source — returns the loaded config model `T`.
pub trait ConfigSource<T>: Send + Sync {
    /// Load + validate the config; `Err` carries a readable message.
    fn load(&self) -> Result<T, String>;
}

/// A config file format — claims extensions, parses text → `T`,
/// renders `T` → text. Generic over the config model `T` so `dyn
/// ConfigFormat<T>` stays object-safe; one `TomlFormat`/`YamlFormat`
/// impl serves every model via a blanket impl.
pub trait ConfigFormat<T>: Send + Sync {
    /// File extensions this format claims (no leading dot), in
    /// preference order — the first wins when several claimed files
    /// coexist in one directory.
    fn extensions(&self) -> &'static [&'static str];

    /// Parse `text` into `T`.
    ///
    /// # Errors
    /// Fails on a syntax or type error.
    fn parse(&self, text: &str) -> Result<T, String>;

    /// Render `config` back to this format's text.
    ///
    /// # Errors
    /// Fails on a serialize error.
    fn render(&self, config: &T) -> Result<String, String>;
}

/// The format claiming `path`'s extension, if any.
#[must_use]
pub fn format_for<'a, T>(
    path: &Path,
    formats: &'a [&'a dyn ConfigFormat<T>],
) -> Option<&'a dyn ConfigFormat<T>> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    formats
        .iter()
        .copied()
        .find(|f| f.extensions().contains(&ext))
}

/// `$HOME`, else `$USERPROFILE`, else `/` — the workspace `home_dir`
/// convention (peregrin-git's `expand_path`).
#[must_use]
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `~` / `~/…` expansion on a path — same rules as peregrin-git's
/// `expand_path` (no shellexpand dep). `~user`, relative, and absolute
/// paths pass through untouched. Non-UTF-8 paths can't start with a
/// literal `~` prefix match and pass through unchanged.
#[must_use]
pub fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        return home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

/// The conventional user config dir for app `name` —
/// `~/.config/<name>` with the tilde expanded at search time.
#[must_use]
pub fn user_config_dir(name: &str) -> PathBuf {
    PathBuf::from("~/.config").join(name)
}

/// Where a file-backed [`ConfigSource`] searches — the generic half of
/// every per-repo `<name>.{yaml,yml,toml}` discovery. Nothing here is
/// hardcoded per app: the basename, env var, and search dirs are all
/// caller-supplied.
#[derive(Clone, Debug)]
pub struct Discovery {
    /// Config file basename — `myco` → `myco.yaml`, `myco.yml`,
    /// `myco.toml`.
    pub name: String,
    /// Env var naming an explicit config file (`$MYCO_CONFIG`); `None`
    /// disables the env step. An explicit `--config`-style path on the
    /// source wins over this.
    pub env_var: Option<String>,
    /// Directories searched in order for `<name>.<ext>` — `~` expands
    /// via [`expand_tilde`]. Convention: `[project dir, user dir]`.
    pub dirs: Vec<PathBuf>,
}

impl Discovery {
    /// Conventional discovery for app `name`: env var `<NAME>_CONFIG`
    /// (uppercased basename), dirs `[., ~/.config/<name>]`.
    #[must_use]
    pub fn for_app(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            env_var: Some(format!("{}_CONFIG", name.to_uppercase())),
            dirs: vec![PathBuf::from("."), user_config_dir(&name)],
            name,
        }
    }

    /// A bare spec — no env step, no dirs; the caller fills both.
    #[must_use]
    pub fn bare(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            env_var: None,
            dirs: Vec::new(),
        }
    }

    /// Override the env var naming an explicit config file.
    #[must_use]
    pub fn env_var(mut self, var: impl Into<String>) -> Self {
        self.env_var = Some(var.into());
        self
    }

    /// Disable the env-var step entirely.
    #[must_use]
    pub fn without_env(mut self) -> Self {
        self.env_var = None;
        self
    }

    /// Replace the search dirs (in order).
    #[must_use]
    pub fn dirs(mut self, dirs: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.dirs = dirs.into_iter().map(Into::into).collect();
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde::Serialize;

    #[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct ToyCfg {
        #[allow(dead_code)]
        name: String,
    }

    #[test]
    fn discovery_for_app_defaults() {
        let d = Discovery::for_app("myco");
        assert_eq!(d.name, "myco");
        assert_eq!(d.env_var.as_deref(), Some("MYCO_CONFIG"));
        assert_eq!(
            d.dirs,
            vec![PathBuf::from("."), PathBuf::from("~/.config/myco")]
        );
        let bare = Discovery::bare("x").dirs(["/tmp"]).without_env();
        assert_eq!(bare.env_var, None);
        assert_eq!(bare.dirs, vec![PathBuf::from("/tmp")]);
        let custom = Discovery::for_app("app").env_var("APP_CFG");
        assert_eq!(custom.env_var.as_deref(), Some("APP_CFG"));
    }

    #[test]
    fn expand_tilde_rules() {
        let home = home_dir();
        assert_eq!(expand_tilde(Path::new("~")), home);
        assert_eq!(expand_tilde(Path::new("~/x/y")), home.join("x/y"));
        assert_eq!(
            expand_tilde(Path::new("/abs/path")),
            PathBuf::from("/abs/path")
        );
        assert_eq!(
            expand_tilde(Path::new("rel/path")),
            PathBuf::from("rel/path")
        );
        assert_eq!(expand_tilde(Path::new("~user/x")), PathBuf::from("~user/x"));
        assert_eq!(user_config_dir("myco"), PathBuf::from("~/.config/myco"));
    }

    #[test]
    fn format_for_matches_extension() {
        struct F(&'static [&'static str]);
        impl ConfigFormat<ToyCfg> for F {
            fn extensions(&self) -> &'static [&'static str] {
                self.0
            }
            fn parse(&self, _text: &str) -> Result<ToyCfg, String> {
                Err("no".into())
            }
            fn render(&self, _config: &ToyCfg) -> Result<String, String> {
                Err("no".into())
            }
        }
        let yaml = F(&["yaml", "yml"]);
        let toml = F(&["toml"]);
        let formats: [&dyn ConfigFormat<ToyCfg>; 2] = [&yaml, &toml];
        assert!(format_for(Path::new("a.yaml"), &formats).is_some());
        assert!(format_for(Path::new("a.yml"), &formats).is_some());
        assert!(format_for(Path::new("a.toml"), &formats).is_some());
        assert!(format_for(Path::new("a.json"), &formats).is_none());
        assert!(format_for(Path::new("noext"), &formats).is_none());
    }
}
