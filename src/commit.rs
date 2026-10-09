//! Atomic commit, clean, dry-run, and orphan scan (Task 8).
//!
//! Normative order (spec §27, summarized): validate every output path
//! including the manifest → compute the create/update/delete diff against the
//! previous manifest (missing prev → deletions disabled) → stage Markdown
//! temps in the destination directory → flush/fsync → atomic rename each →
//! stage/flush/fsync/rename the manifest as the commit record → only then
//! delete obsolete prev-only entries inside the root.
//!
//! Crash semantics: before the manifest rename the previous manifest (if any)
//! stays authoritative and any published-but-uncommitted files are orphan
//! candidates; after the rename the new manifest is authoritative and
//! not-yet-cleaned stale files are orphans. Nothing is ever auto-deleted:
//! leftovers are reported through [`detect_orphans`] as
//! `orphan generated file: <name>` lines for the caller to print on stderr.
//!
//! Commit-record file: [`MANIFEST_FILENAME`] (`manifest.json`) at the output
//! root. It is written by [`commit`] after all Markdown renames and is never
//! deleted by [`commit`] cleanup or [`clean`] (a manifest listing it is
//! treated as hostile and that entry is skipped).
//!
//! Warning channel: [`commit`] cleanup failures (missing file, permission
//! error) keep the `Ok(Plan)` — there is no warnings vector in the return
//! type by design. Callers surface them by running [`detect_orphans`] after
//! the commit and reporting leftovers on stderr (Task 9 `run()` owns this).
//!
//! Run-level manifest handling (no signature exists for it here, so callers
//! implement this three-way branch around [`crate::manifest::read_manifest`]
//! together with [`clean`]): missing manifest → `Ok(vec![])` with zero
//! deletes; invalid manifest → propagate `Err` with zero deletes (never fall
//! back to empty, which would disable deletions silently or, worse, invite
//! deletion without a commit record); valid manifest → [`clean`].
//!
//! Write-side symlink/TOCTOU note (Task 3 review follow-up): path checks are
//! inherently check-then-use on stable Rust (no `openat2`/`RESOLVE_BENEATH`).
//! Mitigations applied here: every destination is validated through
//! [`crate::paths::validate_generated_path`] up front (any failure aborts
//! before the first write) AND re-validated immediately before each
//! rename/delete to shrink the race window; temps use `O_CREAT|O_EXCL` in the
//! destination directory (same filesystem, no cross-device renames);
//! `rename(2)` replaces a destination symlink instead of following it;
//! `remove_file` unlinks without following the final component; the orphan
//! scan never follows symlinks. Residual risk: a privileged local attacker
//! swapping a parent directory for a symlink mid-commit could redirect a
//! rename/delete. The stable-Rust fix is out of scope; callers must pass roots
//! from [`crate::paths::resolve_output_root`] and treat the output tree as
//! trusted during the commit.

use crate::manifest::Manifest;
use crate::paths::validate_generated_path;
use crate::render::RenderedFile;
use crate::{GENERATOR, PROTOCOL_VERSION};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Basename of the commit-record manifest inside the output root.
pub const MANIFEST_FILENAME: &str = "manifest.json";

/// Create/update/delete diff of a render against the previous manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub create: Vec<String>,
    pub update: Vec<String>,
    pub delete: Vec<String>,
}

/// Pure diff, no I/O: desired files absent from `prev` → `create`, present →
/// `update`; prev-listed files absent from desired → `delete`. Missing `prev`
/// (no previous commit record) → `delete` empty: deletions require a valid
/// previous manifest. All three vectors are sorted.
pub fn compute_plan(desired: &[RenderedFile], prev: Option<&Manifest>) -> Plan {
    let prev_set: HashSet<&str> = prev
        .map(|m| m.generated_files.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    let desired_set: HashSet<&str> = desired
        .iter()
        .map(|f| f.rel_path.as_str())
        .collect();
    let mut create = Vec::new();
    let mut update = Vec::new();
    for f in desired {
        if prev_set.contains(f.rel_path.as_str()) {
            update.push(f.rel_path.clone());
        } else {
            create.push(f.rel_path.clone());
        }
    }
    let mut delete = Vec::new();
    if let Some(m) = prev {
        let mut seen = HashSet::new();
        for p in &m.generated_files {
            if !desired_set.contains(p.as_str()) && seen.insert(p.as_str()) {
                delete.push(p.clone());
            }
        }
    }
    create.sort();
    update.sort();
    delete.sort();
    Plan {
        create,
        update,
        delete,
    }
}

/// Best-effort `fsync`: durability hint only, never fails the commit
/// (unsupported on some Windows configurations).
fn sync_best_effort(file: &std::fs::File) {
    let _ = file.sync_all();
}

/// Commit one render atomically. `dry_run` computes the [`Plan`] with zero
/// filesystem effects (no writes, no deletes, the output root is not even
/// created). Otherwise: stage temps in the destination dir → flush/fsync →
/// atomic rename each Markdown → stage/flush/fsync/rename the manifest as the
/// commit record → then delete obsolete prev-only entries inside the root,
/// each re-validated; cleanup failures stay `Ok` (see module docs).
pub fn commit(
    rendered: &[RenderedFile],
    manifest: &Manifest,
    output_root: &Path,
    prev: Option<&Manifest>,
    dry_run: bool,
) -> Result<Plan, String> {
    let plan = compute_plan(rendered, prev);
    if dry_run {
        return Ok(plan);
    }
    // 1. Validate every output path including the manifest BEFORE any write:
    // any failure aborts with zero effects.
    let mut dests: Vec<(String, PathBuf)> = Vec::with_capacity(rendered.len());
    for f in rendered {
        let dest = validate_generated_path(output_root, &f.rel_path)
            .map_err(|e| format!("commit failed: {e}"))?;
        dests.push((f.rel_path.clone(), dest));
    }
    let manifest_dest = validate_generated_path(output_root, MANIFEST_FILENAME)
        .map_err(|e| format!("commit failed: {e}"))?;
    // Serialize the manifest before touching the tree: a serialization
    // failure must also leave zero effects.
    let manifest_bytes = serde_json::to_string_pretty(manifest)
        .map(|mut s| {
            s.push('\n');
            s
        })
        .map_err(|e| format!("commit failed: cannot serialize manifest: {e}"))?;

    // 2. Ensure the root and any nested parent dirs exist.
    std::fs::create_dir_all(output_root)
        .map_err(|e| format!("commit failed: cannot create output root: {e}"))?;
    for (_, dest) in &dests {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("commit failed: cannot create parent dir: {e}"))?;
        }
    }
    // Re-validate after directory creation so later checks observe the tree
    // as the renames will (shrinks the check-then-use window).
    for (rel, _) in &dests {
        validate_generated_path(output_root, rel).map_err(|e| format!("commit failed: {e}"))?;
    }

    // 3. Stage every Markdown temp first (write + flush + fsync, all before
    // any rename), so a crash mid-publish leaves the old manifest
    // authoritative and staged content as orphan candidates.
    let mut staged = Vec::with_capacity(dests.len());
    for (file, (rel, dest)) in rendered.iter().zip(dests.iter()) {
        let parent = dest.parent().unwrap_or(output_root);
        let mut tmp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| format!("commit failed: cannot stage {rel}: {e}"))?;
        tmp.write_all(file.content.as_bytes())
            .map_err(|e| format!("commit failed: cannot stage {rel}: {e}"))?;
        tmp.flush()
            .map_err(|e| format!("commit failed: cannot stage {rel}: {e}"))?;
        sync_best_effort(tmp.as_file());
        staged.push((rel.clone(), tmp, dest.clone()));
    }

    // 4. Atomic rename each Markdown (re-validated immediately before).
    for (rel, tmp, dest) in staged {
        validate_generated_path(output_root, &rel).map_err(|e| format!("commit failed: {e}"))?;
        tmp.persist(&dest)
            .map_err(|e| format!("commit failed: cannot publish {rel}: {}", e.error))?;
    }

    // 5. Stage/flush/fsync/rename the manifest as the commit record.
    let mut mtmp = tempfile::NamedTempFile::new_in(output_root)
        .map_err(|e| format!("commit failed: cannot stage {MANIFEST_FILENAME}: {e}"))?;
    mtmp.write_all(manifest_bytes.as_bytes())
        .map_err(|e| format!("commit failed: cannot stage {MANIFEST_FILENAME}: {e}"))?;
    mtmp.flush()
        .map_err(|e| format!("commit failed: cannot stage {MANIFEST_FILENAME}: {e}"))?;
    sync_best_effort(mtmp.as_file());
    validate_generated_path(output_root, MANIFEST_FILENAME)
        .map_err(|e| format!("commit failed: {e}"))?;
    mtmp
        .persist(&manifest_dest)
        .map_err(|e| format!("commit failed: cannot publish {MANIFEST_FILENAME}: {}", e.error))?;
    // Best-effort directory fsync so the rename itself is durable.
    if let Ok(dir) = std::fs::File::open(output_root) {
        sync_best_effort(&dir);
    }

    // 6. ONLY now delete obsolete prev-only entries inside the root. Each is
    // re-validated (prev-manifest content is untrusted); validation or
    // removal failures are swallowed — the commit stays `Ok` and callers
    // surface leftovers via `detect_orphans`. The commit record itself is
    // never deleted.
    for rel in &plan.delete {
        if rel == MANIFEST_FILENAME {
            continue;
        }
        if let Ok(dest) = validate_generated_path(output_root, rel) {
            let _ = std::fs::remove_file(&dest);
        }
    }
    Ok(plan)
}

/// Delete exactly the manifest-listed entries inside `output_root`, returning
/// the sorted names actually removed. Every entry is re-validated for
/// containment + symlinks before the first delete; any escape aborts with
/// `Err` and zero deletes. Never touches orphans or unrelated files; never
/// deletes the commit-record manifest itself. Already-absent entries are
/// skipped (idempotent: a second run returns empty).
pub fn clean(output_root: &Path, manifest: &Manifest) -> Result<Vec<String>, String> {
    // Validate all entries before the first delete: escape → abort, zero
    // effects from this call attributable to the hostile entry.
    let mut dests: Vec<(String, PathBuf)> = Vec::new();
    let mut seen = HashSet::new();
    for rel in &manifest.generated_files {
        if rel == MANIFEST_FILENAME || !seen.insert(rel.as_str()) {
            continue;
        }
        let dest = validate_generated_path(output_root, rel)
            .map_err(|e| format!("clean failed: {e}"))?;
        dests.push((rel.clone(), dest));
    }
    dests.sort();
    let mut deleted = Vec::new();
    for (rel, dest) in dests {
        match std::fs::remove_file(&dest) {
            Ok(()) => deleted.push(rel),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("clean failed: cannot delete {rel}: {e}")),
        }
    }
    Ok(deleted)
}

/// True iff the file opens with a `---` frontmatter block containing both
/// `generator: cargo-obsidian-graph` and `protocol_version: 1` (YAML-field
/// check only: split on the first `:` per line, trim, exact match).
fn has_generator_marker(path: &Path) -> bool {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let mut lines = text.lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return false,
    }
    let mut generator_ok = false;
    let mut protocol_ok = false;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "generator" if value.trim() == GENERATOR => generator_ok = true,
                "protocol_version" if value.trim() == PROTOCOL_VERSION.to_string() => {
                    protocol_ok = true;
                }
                _ => {}
            }
        }
    }
    generator_ok && protocol_ok
}

/// Report orphan candidates: `.md` files inside `output_root` (symlinks never
/// followed) carrying the generator marker but absent from `prev`. Missing
/// `prev` (no commit record) → every marked file is a candidate. Returns
/// sorted `orphan generated file: <name>` lines (`<name>` is the
/// `/`-normalized root-relative path). Never deletes. A missing or unreadable
/// root yields an empty vec.
pub fn detect_orphans(output_root: &Path, prev: Option<&Manifest>) -> Vec<String> {
    let listed: HashSet<&str> = prev
        .map(|m| m.generated_files.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut stack = vec![output_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let rel = match path.strip_prefix(output_root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if listed.contains(rel.as_str()) {
                continue;
            }
            if has_generator_marker(&path) {
                out.push(format!("orphan generated file: {rel}"));
            }
        }
    }
    out.sort();
    out
}
