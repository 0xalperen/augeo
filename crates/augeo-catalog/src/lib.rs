//! The tweak catalog, embedded in the binary from `catalog/*.toml`.
//!
//! Tweaks are data, so a new one needs no Rust. See [`schema`] for the format, [`lint`] for the rules
//! every entry must pass and [`lock`] for how tweak numbers stay stable across versions.

#![forbid(unsafe_code)]

pub mod lint;
pub mod lock;
pub mod schema;

use augeo_command::{CatalogView, Component, TweakStatus};
use schema::{CatalogFile, Status, Tweak};

/// The embedded catalog files. Each file's name must match the component of its tweaks.
const FILES: &[(&str, &str)] = &[
    ("cpu", include_str!("../../../catalog/cpu.toml")),
    ("system", include_str!("../../../catalog/system.toml")),
];

/// The committed lock file, embedded so tests can compare it with the catalog.
pub const EMBEDDED_LOCK: &str = include_str!("../../../catalog/registry.lock");

/// A loaded catalog.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub tweaks: Vec<Tweak>,
}

impl Catalog {
    /// Parses catalog files given as `(component name, TOML text)`.
    pub fn parse(files: &[(&str, &str)]) -> Result<Self, String> {
        let mut tweaks = Vec::new();
        for (name, text) in files {
            let file: CatalogFile =
                toml::from_str(text).map_err(|e| format!("catalog/{name}.toml: {e}"))?;
            for t in &file.tweak {
                if t.component.as_str() != *name {
                    return Err(format!(
                        "catalog/{name}.toml: {} belongs in {}.toml",
                        t.id, t.component
                    ));
                }
            }
            tweaks.extend(file.tweak);
        }
        tweaks.sort_by_key(|t| (t.component, t.number));
        Ok(Self { tweaks })
    }

    /// The catalog built into this binary.
    pub fn embedded() -> Result<Self, String> {
        Self::parse(FILES)
    }

    pub fn find(&self, component: Component, number: u32) -> Option<&Tweak> {
        self.tweaks
            .iter()
            .find(|t| t.component == component && t.number == number)
    }

    /// Adapts the catalog to the command resolver, with the currently selected game.
    pub fn view(&self, current_game: Option<String>) -> View<'_> {
        View {
            catalog: self,
            current_game,
        }
    }
}

/// The resolver's view of a catalog.
#[derive(Debug)]
pub struct View<'a> {
    catalog: &'a Catalog,
    current_game: Option<String>,
}

impl CatalogView for View<'_> {
    fn numbers(&self, component: Component) -> Vec<u32> {
        self.catalog
            .tweaks
            .iter()
            .filter(|t| t.component == component)
            .map(|t| t.number)
            .collect()
    }

    fn status(&self, component: Component, number: u32) -> Option<TweakStatus> {
        self.catalog
            .find(component, number)
            .map(|t| match t.status {
                Status::Active => TweakStatus::Active,
                Status::Retired => TweakStatus::Retired {
                    reason: t.retired_reason.clone().unwrap_or_default(),
                },
            })
    }

    fn current_game(&self) -> Option<String> {
        self.current_game.clone()
    }
}

#[cfg(test)]
mod tests;
