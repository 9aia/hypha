//! hypha-config-yaml — the YAML [`ConfigFormat`]: claims `.yaml` /
//! `.yml`, serde-driven `parse`/`render` over any config model.
//! YAML is the org-preferred syntax — file discovery tries it before
//! TOML (`hypha-config-file` orders formats by preference). Path
//! resolution/search lives in `hypha-config-file`'s dispatcher; this
//! adapter only owns the YAML syntax.

use hypha_config::ConfigFormat;

/// The YAML config format — a unit value; the blanket impl below
/// serves every serde config model. Claims `yaml` before `yml` so
/// `name.yaml` wins over `name.yml` when both exist in one directory.
#[derive(Clone, Copy, Debug, Default)]
pub struct YamlFormat;

impl<T> ConfigFormat<T> for YamlFormat
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
    }

    fn parse(&self, text: &str) -> Result<T, String> {
        serde_yaml_ng::from_str(text).map_err(|e| format!("parse yaml config: {e}"))
    }

    fn render(&self, config: &T) -> Result<String, String> {
        serde_yaml_ng::to_string(config).map_err(|e| format!("render yaml config: {e}"))
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
        let f = YamlFormat;
        let f: &dyn ConfigFormat<Cfg> = &f;
        assert_eq!(f.extensions(), &["yaml", "yml"]);
        let cfg: Cfg = f.parse("name: app\nport: 8080\n").unwrap();
        assert_eq!(
            cfg,
            Cfg {
                name: "app".into(),
                port: 8080
            }
        );
        let text = f.render(&cfg).unwrap();
        assert_eq!(f.parse(&text).unwrap(), cfg);
        assert!(f.parse("name: [unclosed").is_err());
    }
}
