use crate::metrics::Metric;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontWeight {
    Regular,
    Semibold,
    Bold,
}

impl FontWeight {
    pub fn value(self) -> i32 {
        match self {
            FontWeight::Regular => 400,
            FontWeight::Semibold => 600,
            FontWeight::Bold => 700,
        }
    }
}

/// `font` value meaning "whatever font Windows itself is configured to use".
pub const SYSTEM_FONT: &str = "system";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub metric: Metric,
    #[serde(default = "enabled_true")]
    pub enabled: bool,
    /// Overrides the metric's default label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// "#RRGGBB"; falls back to `text_color`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

fn enabled_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Sampling period in milliseconds.
    pub interval_ms: u64,
    /// "primary" or a monitor name such as "DISPLAY1".
    pub monitor: String,
    pub anchor: Anchor,
    /// Gap in DIPs between the widget and what it is anchored to.
    pub offset: i32,
    /// DIPs kept free at the right of secondary taskbars, which have no
    /// TrayNotifyWnd to measure.
    pub secondary_reserve: i32,
    /// Items stacked per column: 1 is side by side, 2 or more is stacked.
    pub rows: usize,
    /// "system" or a font family name.
    pub font: String,
    pub font_size: f32,
    pub font_weight: FontWeight,
    /// "auto" follows the Windows theme, otherwise "#RRGGBB".
    pub text_color: String,
    /// "transparent" or "#RRGGBBAA".
    pub background: String,
    /// Case-insensitive part of an adapter name. Empty sums all physical ones.
    pub adapter: String,
    pub items: Vec<Item>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            interval_ms: 1000,
            monitor: "primary".into(),
            anchor: Anchor::Right,
            offset: 8,
            secondary_reserve: 190,
            rows: 2,
            font: SYSTEM_FONT.into(),
            font_size: 9.0,
            font_weight: FontWeight::Regular,
            text_color: "auto".into(),
            background: "transparent".into(),
            adapter: String::new(),
            items: Metric::ALL
                .iter()
                .map(|&metric| Item {
                    metric,
                    enabled: metric.default_enabled(),
                    label: None,
                    color: None,
                })
                .collect(),
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let mut cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.normalize()?;
        Ok(cfg)
    }

    /// Clamps ranges, validates colors, drops duplicate metrics and lists any
    /// missing metric as disabled so the menu can always offer it.
    pub fn normalize(&mut self) -> Result<(), String> {
        self.interval_ms = self.interval_ms.clamp(250, 60_000);
        self.rows = self.rows.clamp(1, 3);
        self.font_size = self.font_size.clamp(5.0, 30.0);
        if self.text_color != "auto" {
            Rgba::parse(&self.text_color)?;
        }
        if self.background != "transparent" {
            Rgba::parse(&self.background)?;
        }
        let mut seen = Vec::new();
        self.items.retain(|item| {
            let fresh = !seen.contains(&item.metric);
            seen.push(item.metric);
            fresh
        });
        for item in &self.items {
            if let Some(color) = &item.color {
                Rgba::parse(color)?;
            }
        }
        for &metric in Metric::ALL {
            if !seen.contains(&metric) {
                self.items.push(Item { metric, enabled: false, label: None, color: None });
            }
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        // Written whole and renamed into place: the settings window and the
        // widget process share this file, and neither may read half of it.
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())
    }
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
    base.join("OpenHardwareInfo").join("config.toml")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 0 };

    pub fn parse(text: &str) -> Result<Rgba, String> {
        let bad = || format!("cor inválida: {text:?} (use #RRGGBB ou #RRGGBBAA)");
        let hex = text.strip_prefix('#').ok_or_else(bad)?;
        if !hex.is_ascii() || !(hex.len() == 6 || hex.len() == 8) {
            return Err(bad());
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad());
        let a = if hex.len() == 8 { byte(6)? } else { 255 };
        Ok(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), cfg);
    }

    #[test]
    fn config_from_an_older_version_still_loads() {
        let cfg = Config::parse("font = \"Segoe UI\"\nrows = 2\n").unwrap();
        assert_eq!(cfg.font, "Segoe UI");
        assert_eq!(cfg.font_weight, FontWeight::Regular);
    }

    #[test]
    fn save_replaces_the_file_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("ohi-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.save(&path).unwrap();
        cfg.rows = 1;
        cfg.save(&path).unwrap();
        assert_eq!(Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap().rows, 1);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn partial_items_are_completed_as_disabled() {
        let cfg = Config::parse("[[items]]\nmetric = \"cpu\"\n").unwrap();
        assert_eq!(cfg.items[0].metric, Metric::Cpu);
        assert!(cfg.items[0].enabled);
        assert_eq!(cfg.items.len(), Metric::ALL.len());
        assert!(cfg.items[1..].iter().all(|i| !i.enabled));
    }

    #[test]
    fn duplicates_are_dropped_and_ranges_clamped() {
        let cfg = Config::parse(
            "interval_ms = 1\nrows = 9\n[[items]]\nmetric = \"cpu\"\n[[items]]\nmetric = \"cpu\"\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(cfg.interval_ms, 250);
        assert_eq!(cfg.rows, 3);
        assert_eq!(cfg.items.iter().filter(|i| i.metric == Metric::Cpu).count(), 1);
        assert!(cfg.items[0].enabled);
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(Config::parse("text_color = \"azul\"").is_err());
        assert!(Config::parse("unknown_key = 1").is_err());
        assert!(Config::parse("[[items]]\nmetric = \"cpu_temp\"").is_err());
        assert!(Config::parse("[[items]]\nmetric = \"cpu\"\ncolor = \"#12\"").is_err());
    }

    #[test]
    fn colors_parse() {
        assert_eq!(Rgba::parse("#102030").unwrap(), Rgba { r: 16, g: 32, b: 48, a: 255 });
        assert_eq!(Rgba::parse("#10203040").unwrap().a, 64);
        assert!(Rgba::parse("102030").is_err());
        assert!(Rgba::parse("#éééé").is_err());
    }
}
