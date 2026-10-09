//! Dependency dedup and total-order graph sorting (Task 6).
//!
//! * [`dedup_dependencies`] collapses edges sharing the `(from, to)` key to a
//!   single winner by `ReExport > Use > Module` (ties keep the first edge).
//! * [`sort_graph`] imposes the spec §20 total order: packages by `PackageId`,
//!   targets by `(package, kind-token, name)`, modules by `ModuleId`,
//!   dependencies by `(from, Internal < External, target-string, kind)`,
//!   diagnostics via [`crate::diag::sort_diagnostics`].
//!
//! Comparators are explicit `match`es, never enum discriminants. `HashMap` is
//! used only for point lookups; iteration order never leaks (dedup returns
//! first-seen order, and `sort_graph` fully orders the graph).

use crate::diag::sort_diagnostics;
use crate::domain::{Dependency, DependencyKind, DependencyTarget, WorkspaceGraph};
use std::cmp::Ordering;
use std::collections::HashMap;

/// Rank for dedup precedence and the dependency comparator's kind tier.
fn kind_rank(kind: &DependencyKind) -> u8 {
    match kind {
        DependencyKind::Module => 0,
        DependencyKind::Use => 1,
        DependencyKind::ReExport => 2,
    }
}

/// String key for the `(from, to)` dedup identity. Rust identifiers cannot
/// contain `>`, so the separator is unambiguous.
fn dep_key(dep: &Dependency) -> String {
    let to = match &dep.to {
        DependencyTarget::Internal(m) => format!("i:{}", m.0),
        DependencyTarget::External(e) => format!("e:{}", e.0),
    };
    format!("{}>{to}", dep.from.0)
}

/// Collapse edges sharing the `(from, to)` key to one winner by
/// `ReExport > Use > Module`. Equal-kind duplicates keep the first edge.
/// Output is in first-seen key order (deterministic for a given input).
pub fn dedup_dependencies(deps: Vec<Dependency>) -> Vec<Dependency> {
    let mut order: Vec<String> = Vec::new();
    let mut winners: HashMap<String, Dependency> = HashMap::new();
    for dep in deps {
        let key = dep_key(&dep);
        match winners.get(&key) {
            Some(current) if kind_rank(&current.kind) >= kind_rank(&dep.kind) => {}
            _ => {
                if !winners.contains_key(&key) {
                    order.push(key.clone());
                }
                winners.insert(key, dep);
            }
        }
    }
    order
        .into_iter()
        .map(|key| winners.remove(&key).expect("dedup key just inserted"))
        .collect()
}

/// Explicit dependency comparator: `from` ascending, then
/// `Internal < External`, then the target ID string ascending, then kind
/// `Module < Use < ReExport`, with a source-span tiebreak so the order is
/// total even for same-kind duplicates (which `sort_graph` keeps; only
/// [`dedup_dependencies`] collapses them).
pub fn compare_dependencies(a: &Dependency, b: &Dependency) -> Ordering {
    match a.from.0.cmp(&b.from.0) {
        Ordering::Equal => {}
        other => return other,
    }
    match (&a.to, &b.to) {
        (DependencyTarget::Internal(_), DependencyTarget::External(_)) => {
            return Ordering::Less
        }
        (DependencyTarget::External(_), DependencyTarget::Internal(_)) => {
            return Ordering::Greater
        }
        (DependencyTarget::Internal(x), DependencyTarget::Internal(y)) => {
            match x.0.cmp(&y.0) {
                Ordering::Equal => {}
                other => return other,
            }
        }
        (DependencyTarget::External(x), DependencyTarget::External(y)) => {
            match x.0.cmp(&y.0) {
                Ordering::Equal => {}
                other => return other,
            }
        }
    }
    match kind_rank(&a.kind).cmp(&kind_rank(&b.kind)) {
        Ordering::Equal => {}
        other => return other,
    }
    span_key(&a.source_span).cmp(&span_key(&b.source_span))
}

fn span_key(span: &Option<crate::domain::SourceSpan>) -> (Option<String>, Option<u32>, Option<u32>) {
    match span {
        None => (None, None, None),
        Some(s) => (
            Some(s.file.to_string_lossy().replace('\\', "/")),
            Some(s.line_start),
            Some(s.line_end),
        ),
    }
}

/// Sort every graph boundary into its total order (spec §20). Diagnostics go
/// through [`sort_diagnostics`].
pub fn sort_graph(graph: &mut WorkspaceGraph) {
    graph.packages.sort_by(|a, b| a.id.cmp(&b.id));
    graph.targets.sort_by(|a, b| {
        (
            &a.package_id,
            a.kind.kind_token(),
            &a.name,
        )
            .cmp(&(&b.package_id, b.kind.kind_token(), &b.name))
    });
    graph.modules.sort_by(|a, b| a.id.cmp(&b.id));
    graph.dependencies.sort_by(compare_dependencies);
    sort_diagnostics(&mut graph.diagnostics);
}
