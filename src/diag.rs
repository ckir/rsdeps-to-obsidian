//! Diagnostics ordering and workspace-relative paths (Task 6).
//!
//! * [`sort_diagnostics`] imposes the spec §22 total order: severity
//!   `Info < Warning < Error`, then code, then `/`-normalized file path
//!   (missing span first), then line (missing first), then line-end, then
//!   message. The severity rank is an explicit `match`, never an enum
//!   discriminant.
//! * [`workspace_rel`] renders a path workspace-root-relative with `/`
//!   separators and no `.`/`..` segments (lexical normalization only; the
//!   filesystem is never touched).

use crate::domain::{Diagnostic, DiagnosticSeverity, SourceSpan};
use std::cmp::Ordering;
use std::path::{Component, Path};

/// Explicit severity rank for the diagnostic comparator.
fn severity_rank(severity: &DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Info => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Error => 2,
    }
}

/// `/`-normalized file key for ordering. `None` (no span) sorts first.
fn path_key(span: Option<&SourceSpan>) -> Option<String> {
    span.map(|s| s.file.to_string_lossy().replace('\\', "/"))
}

/// Sort diagnostics into their total order (spec §22).
///
/// The `&mut Vec` shape is the Task 6 interface contract (not a slice).
#[allow(clippy::ptr_arg)]
pub fn sort_diagnostics(diags: &mut Vec<Diagnostic>) {
    diags.sort_by(|a, b| {
        match severity_rank(&a.severity).cmp(&severity_rank(&b.severity)) {
            Ordering::Equal => {}
            other => return other,
        }
        match a.code.cmp(&b.code) {
            Ordering::Equal => {}
            other => return other,
        }
        match path_key(a.source_span.as_ref()).cmp(&path_key(b.source_span.as_ref())) {
            Ordering::Equal => {}
            other => return other,
        }
        let line_a = a.source_span.as_ref().map(|s| s.line_start);
        let line_b = b.source_span.as_ref().map(|s| s.line_start);
        match line_a.cmp(&line_b) {
            Ordering::Equal => {}
            other => return other,
        }
        let end_a = a.source_span.as_ref().map(|s| s.line_end);
        let end_b = b.source_span.as_ref().map(|s| s.line_end);
        match end_a.cmp(&end_b) {
            Ordering::Equal => {}
            other => return other,
        }
        a.message.cmp(&b.message)
    });
}

/// Lexically resolve `.` / `..` without touching the filesystem.
fn lex_normalize(path: &Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Render `path` relative to workspace `root` with `/` separators and no
/// `.`/`..` segments. A path equal to `root` yields `""`. A path outside
/// `root` is returned lexically normalized with `/` separators (absolute
/// paths keep their leading `/`).
pub fn workspace_rel(path: &Path, root: &Path) -> String {
    let normalized = lex_normalize(path);
    let normalized_root = lex_normalize(root);
    let rel = normalized
        .strip_prefix(&normalized_root)
        .map(Path::to_path_buf)
        .unwrap_or(normalized);
    let mut parts: Vec<String> = Vec::new();
    let mut rooted = false;
    for component in rel.components() {
        match component {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned());
            }
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            // After normalization `..` only survives for relative paths that
            // escape their start; keep it verbatim (nothing better is lexical).
            Component::ParentDir => parts.push("..".to_string()),
            Component::Normal(part) => {
                parts.push(part.to_string_lossy().into_owned());
            }
        }
    }
    let mut out = parts.join("/");
    // A Windows `Prefix` (e.g. `C:`) already anchors the path; only bare
    // Unix roots need the re-added leading slash.
    let anchored = parts.first().is_some_and(|p| p.ends_with(':'));
    if rooted && !anchored {
        out.insert(0, '/');
    }
    out
}
