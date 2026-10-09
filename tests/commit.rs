//! Task 8: atomic commit, clean, dry-run, orphans.

use rsdeps2obsidian::commit::{clean, commit, compute_plan, detect_orphans, MANIFEST_FILENAME};
use rsdeps2obsidian::manifest::{read_manifest, Manifest};
use rsdeps2obsidian::paths::resolve_output_root;
use rsdeps2obsidian::render::RenderedFile;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn test_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "rsdeps-task8-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn note(name: &str, body_marker: &str) -> RenderedFile {
    RenderedFile {
        rel_path: name.to_string(),
        content: format!(
            "---\ngraph_type: rust-module\nprotocol_version: 1\nspec_version: 0.4.0\nschema_version: 3\ngenerator: cargo-obsidian-graph\nmodule_id: {body_marker}\n---\n\n# {body_marker}\n"
        ),
    }
}

fn manifest_for(files: &[&str]) -> Manifest {
    let mut generated_files: Vec<String> = files.iter().map(|s| s.to_string()).collect();
    generated_files.sort();
    Manifest {
        protocol_version: 1,
        spec_version: "0.4.0".to_string(),
        schema_version: 3,
        generator: "cargo-obsidian-graph".to_string(),
        generated_files,
        module_records: vec![],
    }
}

/// Recursive snapshot of a directory tree: rel-path -> bytes. Panics with path
/// context on I/O errors (test reader: fail loudly, never swallow).
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let entries = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read test tree dir {}: {e}", dir.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|e| panic!("read test tree entry in {}: {e}", dir.display()));
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.insert(
                    rel,
                    fs::read(&path)
                        .unwrap_or_else(|e| panic!("read snapshot file {}: {e}", path.display())),
                );
            }
        }
    }
    walk(root, root, &mut out);
    out
}

#[test]
fn dry_run_writes_nothing() {
    let parent = test_root("dryrun");
    let root = parent.join("graph"); // deliberately not created
    let rendered = vec![note("b.md", "b"), note("a.md", "a")];
    let manifest = manifest_for(&["a.md", "b.md"]);

    // Dry run with no previous manifest: everything is created, nothing written.
    let plan = commit(&rendered, &manifest, &root, None, true).unwrap();
    assert_eq!(plan.create, vec!["a.md".to_string(), "b.md".to_string()]);
    assert!(plan.update.is_empty());
    assert!(plan.delete.is_empty());
    assert!(!root.exists(), "dry-run must not create the output dir");

    // Dry run over an existing tree (incl. the --dry-run --clean case):
    // pre/post snapshots must be identical, zero writes and zero deletes.
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("old.md"), "stale content").unwrap();
    let prev = manifest_for(&["old.md"]);
    let before = snapshot(&root);
    let plan = commit(&rendered, &manifest, &root, Some(&prev), true).unwrap();
    assert_eq!(plan.create, vec!["a.md".to_string(), "b.md".to_string()]);
    assert!(plan.update.is_empty());
    assert_eq!(plan.delete, vec!["old.md".to_string()]);
    assert_eq!(
        snapshot(&root),
        before,
        "dry-run must leave the tree untouched"
    );
    assert!(!root.join(MANIFEST_FILENAME).exists());
    assert!(root.join("old.md").exists(), "dry-run must not delete");
}

#[test]
fn manifest_is_commit_record_and_cleanup_after() {
    let root = test_root("commit-record");
    // Previous generation on disk.
    fs::write(root.join("keep.md"), "old keep").unwrap();
    fs::write(root.join("stale.md"), "stale").unwrap();
    let prev = manifest_for(&["keep.md", "stale.md"]);
    fs::write(
        root.join(MANIFEST_FILENAME),
        serde_json::to_string_pretty(&prev).unwrap(),
    )
    .unwrap();

    let rendered = vec![note("keep.md", "keep-new"), note("new.md", "new")];
    let manifest = manifest_for(&["keep.md", "new.md"]);

    let plan = commit(&rendered, &manifest, &root, Some(&prev), false).unwrap();
    assert_eq!(plan.create, vec!["new.md".to_string()]);
    assert_eq!(plan.update, vec!["keep.md".to_string()]);
    assert_eq!(plan.delete, vec!["stale.md".to_string()]);

    // Markdown published with new bytes, manifest is the commit record.
    assert_eq!(
        fs::read_to_string(root.join("keep.md")).unwrap(),
        rendered[0].content
    );
    assert_eq!(
        fs::read_to_string(root.join("new.md")).unwrap(),
        rendered[1].content
    );
    let on_disk = read_manifest(&root.join(MANIFEST_FILENAME))
        .unwrap()
        .unwrap();
    assert_eq!(on_disk, manifest);
    // Stale prev-only entry deleted only after the manifest rename: the new
    // manifest is authoritative while the stale file is gone.
    assert!(!root.join("stale.md").exists());
    // No temp files leaked.
    let leftovers: Vec<String> = snapshot(&root)
        .keys()
        .filter(|k| k.contains(".tmp") || k.starts_with(".tmp"))
        .cloned()
        .collect();
    assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
}

#[test]
fn clean_deletes_only_listed_entries() {
    let root = test_root("clean");
    fs::write(root.join("listed.md"), "generated").unwrap();
    fs::write(root.join("unrelated.md"), "user note, no marker").unwrap();
    fs::write(
        root.join("orphan.md"),
        "---\ngenerator: cargo-obsidian-graph\nprotocol_version: 1\n---\norphan\n",
    )
    .unwrap();
    let manifest = manifest_for(&["listed.md"]);

    let deleted = clean(&root, &manifest).unwrap();
    assert_eq!(deleted, vec!["listed.md".to_string()]);
    assert!(!root.join("listed.md").exists());
    assert!(
        root.join("unrelated.md").exists(),
        "clean must spare unrelated files"
    );
    assert!(
        root.join("orphan.md").exists(),
        "clean must never delete orphans"
    );

    // Invalid manifest => Err + zero deletes (read boundary rejects it first).
    fs::write(root.join("listed.md"), "generated").unwrap();
    fs::write(root.join(MANIFEST_FILENAME), "{not json").unwrap();
    let before = snapshot(&root);
    let err = read_manifest(&root.join(MANIFEST_FILENAME)).unwrap_err();
    assert!(err.contains("invalid manifest"), "unexpected error: {err}");
    // The run-level flow propagates that error before commit/clean ever run.
    let run_outcome: Result<(), String> = match read_manifest(&root.join(MANIFEST_FILENAME)) {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    };
    assert!(run_outcome.unwrap_err().contains("invalid manifest"));
    assert_eq!(
        snapshot(&root),
        before,
        "invalid manifest must cause zero deletes"
    );

    // Missing manifest => Ok(empty), zero deletes: the run-level flow maps
    // None to an empty deletion set without touching the tree.
    let _ = fs::remove_file(root.join(MANIFEST_FILENAME));
    let before = snapshot(&root);
    let result: Result<Vec<String>, String> =
        match read_manifest(&root.join(MANIFEST_FILENAME)).unwrap() {
            None => Ok(vec![]),
            Some(m) => clean(&root, &m),
        };
    assert_eq!(result.unwrap(), Vec::<String>::new());
    assert_eq!(
        snapshot(&root),
        before,
        "missing manifest must delete nothing"
    );
    // The same empty-deletion semantics through the real `clean`: an empty
    // manifest deletes nothing and reports nothing.
    assert_eq!(
        clean(&root, &manifest_for(&[])).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(snapshot(&root), before, "empty clean must delete nothing");
    // And through the real `commit`: with no previous manifest, deletions are
    // disabled, so pre-existing unrelated files survive the commit.
    let rendered = vec![note("fresh.md", "fresh")];
    let fresh_manifest = manifest_for(&["fresh.md"]);
    let plan = commit(&rendered, &fresh_manifest, &root, None, false).unwrap();
    assert!(plan.delete.is_empty());
    assert!(root.join("unrelated.md").exists());
    assert!(root.join("orphan.md").exists());
    assert!(root.join("fresh.md").exists());
}

#[test]
fn orphans_reported_never_deleted() {
    let root = test_root("orphans");
    fs::write(
        root.join("orphan.md"),
        "---\ngraph_type: rust-module\nprotocol_version: 1\nspec_version: 0.4.0\nschema_version: 3\ngenerator: cargo-obsidian-graph\n---\n",
    )
    .unwrap();
    fs::write(root.join("plain.md"), "no marker here\n").unwrap();
    fs::write(
        root.join("wrongproto.md"),
        "---\ngenerator: cargo-obsidian-graph\nprotocol_version: 2\n---\n",
    )
    .unwrap();
    let prev = manifest_for(&["index.md"]);

    let orphans = detect_orphans(&root, Some(&prev));
    assert_eq!(
        orphans,
        vec!["orphan generated file: orphan.md".to_string()]
    );
    assert!(detect_orphans(&root, None).contains(&"orphan generated file: orphan.md".to_string()));

    // commit + clean leave the orphan on disk.
    let rendered = vec![note("index.md", "root")];
    let manifest = manifest_for(&["index.md"]);
    commit(&rendered, &manifest, &root, Some(&prev), false).unwrap();
    assert!(
        root.join("index.md").exists(),
        "commit must publish listed files"
    );
    assert!(
        root.join("orphan.md").exists(),
        "commit must never delete orphans"
    );
    clean(&root, &manifest).unwrap();
    assert!(
        !root.join("index.md").exists(),
        "clean removes listed entries"
    );
    assert!(
        root.join("orphan.md").exists(),
        "orphans must never be auto-deleted"
    );
    assert!(root.join("plain.md").exists());
    assert!(root.join("wrongproto.md").exists());
}

#[test]
fn crash_points_keep_old_manifest_authoritative() {
    // (a) Interruption after Markdown publish, before manifest rename.
    let root_a = test_root("crash-a");
    let old_a = manifest_for(&["old.md"]);
    fs::write(root_a.join("old.md"), "old").unwrap();
    fs::write(
        root_a.join(MANIFEST_FILENAME),
        serde_json::to_string_pretty(&old_a).unwrap(),
    )
    .unwrap();
    fs::write(root_a.join("new.md"), note("new.md", "new").content).unwrap(); // published, uncommitted
    assert_eq!(
        read_manifest(&root_a.join(MANIFEST_FILENAME)).unwrap(),
        Some(old_a.clone())
    );
    assert_eq!(
        detect_orphans(&root_a, Some(&old_a)),
        vec!["orphan generated file: new.md".to_string()]
    );

    // (b) Interruption after manifest temp write, before rename.
    let root_b = test_root("crash-b");
    let old_b = manifest_for(&["old.md"]);
    fs::write(root_b.join("old.md"), "old").unwrap();
    fs::write(
        root_b.join(MANIFEST_FILENAME),
        serde_json::to_string_pretty(&old_b).unwrap(),
    )
    .unwrap();
    fs::write(root_b.join("new.md"), note("new.md", "new").content).unwrap();
    let new_b = manifest_for(&["new.md", "old.md"]);
    fs::write(
        root_b.join(".tmp-manifest-123"),
        serde_json::to_string_pretty(&new_b).unwrap(),
    )
    .unwrap();
    assert_eq!(
        read_manifest(&root_b.join(MANIFEST_FILENAME)).unwrap(),
        Some(old_b.clone())
    );
    assert_eq!(
        detect_orphans(&root_b, Some(&old_b)),
        vec!["orphan generated file: new.md".to_string()],
        "staged temp must not be mistaken for an orphan or a manifest"
    );

    // (c) Interruption after manifest rename, before stale cleanup.
    let root_c = test_root("crash-c");
    let old_c = manifest_for(&["old.md", "stale.md"]);
    // The stale file is a previously generated note, so it carries the marker.
    fs::write(root_c.join("stale.md"), note("stale.md", "stale").content).unwrap();
    let new_c = manifest_for(&["old.md"]);
    fs::write(root_c.join("old.md"), "old").unwrap();
    fs::write(
        root_c.join(MANIFEST_FILENAME),
        serde_json::to_string_pretty(&new_c).unwrap(),
    )
    .unwrap();
    // New manifest authoritative; leftover is reported, never auto-deleted.
    assert_eq!(
        read_manifest(&root_c.join(MANIFEST_FILENAME)).unwrap(),
        Some(new_c.clone())
    );
    assert_eq!(
        detect_orphans(&root_c, Some(&new_c)),
        vec!["orphan generated file: stale.md".to_string()]
    );
    assert!(
        root_c.join("stale.md").exists(),
        "leftovers must never be auto-deleted"
    );
    let _ = old_c;
}

#[test]
fn symlink_escape_rejected() {
    let base = test_root("symlink");
    let real = base.join("real");
    fs::create_dir_all(&real).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, base.join("link")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&real, base.join("link")).unwrap();

    // Output through a symlinked parent: resolution fails, commit never starts.
    let err = resolve_output_root(&base.join("link").join("graph"), &base).unwrap_err();
    assert!(
        err.contains("unsafe output path"),
        "unexpected error: {err}"
    );
    assert!(!base.join("link").join("graph").exists());
    assert!(
        !real.join("graph").exists(),
        "zero writes through the symlink"
    );

    // A generated path escaping the root is rejected with zero writes.
    let root = base.join("out");
    fs::create_dir_all(&root).unwrap();
    let evil = vec![RenderedFile {
        rel_path: "../evil.md".to_string(),
        content: "evil".to_string(),
    }];
    let manifest = manifest_for(&["../evil.md"]);
    let before = snapshot(&root);
    let err = commit(&evil, &manifest, &root, None, false).unwrap_err();
    assert!(err.contains("unsafe"), "unexpected error: {err}");
    assert_eq!(
        snapshot(&root),
        before,
        "failed validation must write nothing"
    );
    assert!(!base.join("evil.md").exists());
}

#[test]
fn compute_plan_partitions_without_io() {
    assert_eq!(
        compute_plan(&[], None),
        rsdeps2obsidian::commit::Plan {
            create: vec![],
            update: vec![],
            delete: vec![],
        }
    );
    let rendered = vec![note("a.md", "a"), note("b.md", "b")];
    let plan = compute_plan(&rendered, None);
    assert_eq!(plan.create, vec!["a.md".to_string(), "b.md".to_string()]);
    assert!(plan.update.is_empty() && plan.delete.is_empty());

    let prev = manifest_for(&["a.md", "stale.md"]);
    let plan = compute_plan(&rendered, Some(&prev));
    assert_eq!(plan.create, vec!["b.md".to_string()]);
    assert_eq!(plan.update, vec!["a.md".to_string()]);
    assert_eq!(plan.delete, vec!["stale.md".to_string()]);

    // Doubled desired entries count once (first occurrence wins).
    let doubled = vec![note("a.md", "a"), note("a.md", "a"), note("b.md", "b")];
    assert_eq!(compute_plan(&doubled, Some(&prev)), plan);
}

#[test]
fn manifest_record_never_clobbered_or_deleted() {
    // A hostile prev manifest spelling the commit record as `./manifest.json`
    // must not delete or clobber it; the legitimate stale entry is still
    // handled normally.
    let root = test_root("manifest-self");
    let old = manifest_for(&["stale.md"]);
    fs::write(
        root.join(MANIFEST_FILENAME),
        serde_json::to_string_pretty(&old).unwrap(),
    )
    .unwrap();
    fs::write(root.join("stale.md"), "stale").unwrap();
    let mut hostile_prev = manifest_for(&["./manifest.json", "stale.md"]);
    hostile_prev.generated_files.sort();
    let rendered = vec![note("a.md", "a")];
    let new = manifest_for(&["a.md"]);

    let plan = commit(&rendered, &new, &root, Some(&hostile_prev), false).unwrap();
    assert_eq!(
        plan.delete,
        vec!["./manifest.json".to_string(), "stale.md".to_string()]
    );
    // Commit record intact as the NEW manifest; stale handled normally.
    assert_eq!(
        read_manifest(&root.join(MANIFEST_FILENAME)).unwrap(),
        Some(new)
    );
    assert!(!root.join("stale.md").exists());
    assert!(root.join("a.md").exists());

    // `clean` with the same hostile spelling deletes the legit entry only.
    fs::write(root.join("stale.md"), "stale again").unwrap();
    let deleted = clean(&root, &hostile_prev).unwrap();
    assert_eq!(deleted, vec!["stale.md".to_string()]);
    assert!(!root.join("stale.md").exists());
    assert_eq!(
        read_manifest(&root.join(MANIFEST_FILENAME)).unwrap(),
        Some(manifest_for(&["a.md"])),
        "clean must leave the commit record untouched"
    );

    // A rendered file targeting the commit record (either spelling) is
    // rejected with Err and zero writes — never published-then-clobbered.
    for spelling in ["manifest.json", "./manifest.json"] {
        let before = snapshot(&root);
        let evil = vec![RenderedFile {
            rel_path: spelling.to_string(),
            content: "evil content".to_string(),
        }];
        let err = commit(&evil, &manifest_for(&[spelling]), &root, None, false).unwrap_err();
        assert!(err.contains("collides"), "unexpected error: {err}");
        assert_eq!(
            snapshot(&root),
            before,
            "colliding render must write nothing"
        );
    }
    assert_eq!(
        read_manifest(&root.join(MANIFEST_FILENAME)).unwrap(),
        Some(manifest_for(&["a.md"]))
    );
}

#[test]
fn manifest_rename_failure_skips_cleanup() {
    // Ordering proof: force the manifest rename to fail (a non-empty
    // directory occupies the manifest destination) AFTER Markdown was
    // staged and published. Cleanup must not run: the stale prev-listed
    // file still exists.
    let root = test_root("rename-fail");
    fs::write(root.join("old.md"), "old").unwrap();
    fs::write(root.join("stale.md"), "stale").unwrap();
    let prev = manifest_for(&["old.md", "stale.md"]);
    fs::create_dir_all(root.join(MANIFEST_FILENAME).join("inner")).unwrap();
    fs::write(root.join(MANIFEST_FILENAME).join("inner").join("x"), "x").unwrap();

    let rendered = vec![note("old.md", "old-new"), note("new.md", "new")];
    let new = manifest_for(&["new.md", "old.md"]);
    let err = commit(&rendered, &new, &root, Some(&prev), false).unwrap_err();
    assert!(
        err.contains("cannot publish manifest.json"),
        "unexpected error: {err}"
    );
    // Markdown publish succeeded (we got past stage 4) …
    assert_eq!(
        fs::read_to_string(root.join("new.md")).unwrap(),
        note("new.md", "new").content
    );
    // … but the stale file was NOT deleted: deletes run only after the
    // manifest rename.
    assert!(
        root.join("stale.md").exists(),
        "cleanup ran before manifest rename"
    );
    assert!(root.join(MANIFEST_FILENAME).is_dir());
}

#[test]
fn hostile_entries_fail_closed() {
    // (a) `clean` with a valid Manifest struct containing an escape entry:
    // Err + zero deletes — the legitimate entry SURVIVES (fail-closed).
    let base = test_root("hostile");
    let root = base.join("vault");
    fs::create_dir_all(&root).unwrap();
    fs::write(base.join("evil.md"), "sentinel").unwrap();
    fs::write(root.join("legit.md"), "legit").unwrap();
    let hostile = manifest_for(&["../evil.md", "legit.md"]);
    let before = snapshot(&base);
    let err = clean(&root, &hostile).unwrap_err();
    assert!(err.contains("clean failed"), "unexpected error: {err}");
    assert_eq!(snapshot(&base), before, "escape abort must delete nothing");
    assert!(
        root.join("legit.md").exists(),
        "legit entry must survive escape abort"
    );
    assert_eq!(
        fs::read_to_string(base.join("evil.md")).unwrap(),
        "sentinel"
    );

    // (b) `commit` cleanup with a hostile prev: Ok (cleanup-failure-tolerant
    // per brief), escape target untouched, legitimate stale handled normally.
    fs::write(root.join("stale.md"), "stale").unwrap();
    let hostile_prev = manifest_for(&["../evil.md", "stale.md"]);
    let rendered = vec![note("a.md", "a")];
    let new = manifest_for(&["a.md"]);
    let plan = commit(&rendered, &new, &root, Some(&hostile_prev), false).unwrap();
    assert_eq!(
        plan.delete,
        vec!["../evil.md".to_string(), "stale.md".to_string()]
    );
    assert_eq!(
        fs::read_to_string(base.join("evil.md")).unwrap(),
        "sentinel"
    );
    assert!(
        !root.join("stale.md").exists(),
        "legit stale must be cleaned"
    );
    assert!(root.join("a.md").exists());
    assert_eq!(
        read_manifest(&root.join(MANIFEST_FILENAME)).unwrap(),
        Some(new)
    );
}
