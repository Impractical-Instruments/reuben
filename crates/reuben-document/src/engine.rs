//! The [`Engine`] is `reuben-core`'s; what lives here is the one step of building one that needs a
//! document. A free function rather than a constructor because the type is defined below this
//! crate.
//!
//! see rules: authoring-library

use reuben_core::{AudioConfig, Engine, Plan, PlanError, Registry};

use crate::format::{load_instrument, LoadError, LoadWarning};
use crate::resources::ResourceResolver;

/// Why [`from_document`] failed: the document didn't load, or the loaded graph
/// didn't instantiate.
#[derive(Debug)]
pub enum FromDocumentError {
    /// The instrument document failed to parse/build (see [`LoadError`]).
    Load(LoadError),
    /// The loaded graph failed to instantiate into a [`Plan`] (see [`PlanError`]).
    Plan(PlanError),
}

impl std::fmt::Display for FromDocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FromDocumentError::Load(e) => write!(f, "load instrument: {e}"),
            // PlanError has no Display of its own; its Debug form is the diagnostic.
            FromDocumentError::Plan(e) => write!(f, "instantiate plan: {e:?}"),
        }
    }
}

impl std::error::Error for FromDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FromDocumentError::Load(e) => Some(e),
            FromDocumentError::Plan(_) => None,
        }
    }
}

/// Construct an engine straight from an instrument document: `load_instrument` →
/// [`Plan::instantiate`] → `Engine::new`. The one place this glue is written — every
/// shell (native, web, game) calls it instead of re-wiring the chain. Returns the load
/// warnings alongside the engine: resource problems are non-fatal but they are
/// authoring errors the shell must surface, not swallow.
pub fn from_document(
    text: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    config: AudioConfig,
) -> Result<(Engine, Vec<LoadWarning>), FromDocumentError> {
    let loaded = load_instrument(text, registry, resolver).map_err(FromDocumentError::Load)?;
    let plan = Plan::instantiate(loaded.graph, config).map_err(FromDocumentError::Plan)?;
    Ok((Engine::new(plan), loaded.warnings))
}
