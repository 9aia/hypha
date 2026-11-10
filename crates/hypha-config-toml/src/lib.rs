//! hypha-config-toml — the TOML [`ConfigFormat`]: claims `.toml`,
//! serde-driven `parse`/`render` over any config model. Path
//! resolution/search lives in `hypha-config-file`'s dispatcher; this
//! adapter only owns the TOML syntax.

use hypha_config::ConfigFormat;

/// The TOML config format — a unit value; the blanket impl below
/// serves every serde config model.
#[derive(Clone, Copy, Debug, Default)]
pub struct TomlFormat;

impl<T> ConfigFormat<T> for TomlFormat
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
    }

    fn parse(&self, text: &str) -> Result<T, String> {
        toml::from_str(text).map_err(|e| format!("parse toml config: {e}"))
    }

    fn render(&self, config: &T) -> Result<String, String> {
        toml::to_string(config).map_err(|e| format!("render toml config: {e}"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde::Serialize;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Cfg {
        name: String,
        port: u16,
    }

    #[test]
    fn parse_and_render_round_trip() {
        let f = TomlFormat;
        let f: &dyn ConfigFormat<Cfg> = &f;
        assert_eq!(f.extensions(), &["toml"]);
        let cfg: Cfg = f.parse("name = \"app\"\nport = 8080\n").unwrap();
        assert_eq!(
            cfg,
            Cfg {
                name: "app".into(),
                port: 8080
            }
        );
        let text = f.render(&cfg).unwrap();
        assert_eq!(f.parse(&text).unwrap(), cfg);
        assert!(f.parse("name = [unclosed").is_err());
    }
}
