use crate::config::Config;
use crate::domain::{Package, Target, TargetKind};
use crate::ids::{make_target_id, PackageId};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalDep {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredWorkspace {
    pub root: PathBuf,
    pub packages: Vec<Package>,
    pub targets: Vec<Target>,
    pub external_deps: Vec<ExternalDep>,
}

fn target_kind(kinds: &[cargo_metadata::TargetKind]) -> Option<TargetKind> {
    use cargo_metadata::TargetKind as Ck;
    if kinds.iter().any(|k| matches!(k, Ck::Lib)) {
        Some(TargetKind::Lib)
    } else if kinds.iter().any(|k| matches!(k, Ck::Bin)) {
        Some(TargetKind::Bin)
    } else {
        None
    }
}

pub fn discover(config: &Config) -> Result<DiscoveredWorkspace, String> {
    // Single metadata invocation. cargo_metadata always issues
    // `cargo metadata --format-version 1` (hardcoded in MetadataCommand);
    // there is no separate format-version setter to configure.
    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(manifest) = &config.manifest_path {
        cmd.manifest_path(manifest);
    } else {
        cmd.current_dir(&config.workspace_input);
    }
    let metadata = cmd.exec().map_err(|e| e.to_string())?;

    let members: HashSet<&cargo_metadata::PackageId> =
        metadata.workspace_members.iter().collect();

    let mut packages = Vec::new();
    let mut targets = Vec::new();
    for pkg in &metadata.packages {
        if !members.contains(&pkg.id) {
            continue;
        }
        let package_id = PackageId(pkg.name.to_string());
        for target in &pkg.targets {
            let Some(kind) = target_kind(&target.kind) else {
                continue;
            };
            let id = make_target_id(&pkg.name, kind.kind_token(), &target.name)?;
            targets.push(Target {
                id,
                package_id: package_id.clone(),
                name: target.name.clone(),
                kind,
                root_source: target.src_path.clone().into_std_path_buf(),
            });
        }
        packages.push(Package {
            id: package_id,
            name: pkg.name.to_string(),
            manifest_path: pkg.manifest_path.clone().into_std_path_buf(),
        });
    }
    packages.sort_by(|a, b| a.id.cmp(&b.id));
    targets.sort_by(|a, b| {
        (
            &a.package_id,
            a.kind.kind_token(),
            &a.name,
        )
            .cmp(&(&b.package_id, b.kind.kind_token(), &b.name))
    });

    let mut external_deps = Vec::new();
    if let Some(resolve) = &metadata.resolve {
        let by_id: HashMap<&cargo_metadata::PackageId, &cargo_metadata::Package> =
            metadata.packages.iter().map(|p| (&p.id, p)).collect();
        let mut seen = HashSet::new();
        for node in &resolve.nodes {
            if members.contains(&node.id) {
                continue;
            }
            // Identity is the resolved package's own name@version;
            // a dependency rename/alias never participates.
            if let Some(pkg) = by_id.get(&node.id) {
                if seen.insert(pkg.id.clone()) {
                    external_deps.push(ExternalDep {
                        name: pkg.name.to_string(),
                        version: pkg.version.to_string(),
                    });
                }
            }
        }
    }
    external_deps.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));

    Ok(DiscoveredWorkspace {
        root: metadata.workspace_root.into_std_path_buf(),
        packages,
        targets,
        external_deps,
    })
}
