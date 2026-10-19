//! Filtering: combines path patterns and attribute predicates into a single
//! [`FilterSet`] used by the scanner to decide traversal and emission.

pub mod pattern;
pub mod predicate;

pub use pattern::PatternSet;
pub use predicate::{Predicate, is_subscribable};

use opcua::types::NodeId;

use crate::cli::FilterArgs;
use crate::error::Result;
use crate::opcua::attributes::NodeAttributes;
use crate::opcua::namespaces::NamespaceTable;

/// All filters required to traverse and emit nodes.
pub struct FilterSet {
    include: PatternSet,
    exclude: PatternSet,
    predicate: Predicate,
}

impl FilterSet {
    /// Compile a filter set from CLI filter arguments.
    pub fn compile(filters: &FilterArgs, namespaces: &NamespaceTable) -> Result<Self> {
        let include = PatternSet::compile(&filters.include, &filters.include_regex)?;
        let exclude = PatternSet::compile(&filters.exclude, &filters.exclude_regex)?;
        let predicate = Predicate::compile(filters, namespaces)?;
        Ok(Self {
            include,
            exclude,
            predicate,
        })
    }

    /// Whether the path-level filters alone permit descending into a branch.
    /// (Attribute predicates do not affect traversal, only emission.)
    pub fn path_decision(&self, path: &str) -> PathDecision {
        if self.exclude.matches(path) {
            return PathDecision {
                descend: false,
                path_included: false,
                pruned: true,
            };
        }
        let descend = self.include.prefix_compatible(path);
        let path_included = self.include.is_empty() || self.include.matches(path);
        PathDecision {
            descend,
            path_included,
            pruned: false,
        }
    }

    /// Whether a node passes the attribute-level predicates.
    pub fn attributes_match(
        &self,
        node_id: &NodeId,
        attrs: &NodeAttributes,
        namespaces: &NamespaceTable,
    ) -> bool {
        self.predicate.matches(node_id, attrs, namespaces)
    }
}

/// Result of evaluating path-level filters.
#[derive(Debug, Clone, Copy)]
pub struct PathDecision {
    /// Whether to browse the node's children.
    pub descend: bool,
    /// Whether the path matches the include rules (still subject to attributes).
    pub path_included: bool,
    /// Whether the subtree was pruned by an exclude rule.
    pub pruned: bool,
}
