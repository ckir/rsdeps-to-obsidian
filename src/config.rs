use clap::Parser;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "rsdeps2obsidian",
    version,
    about = "Cargo workspace module-structure analysis to Obsidian graph"
)]
pub struct CliArgs {
    #[arg(value_name = "WORKSPACE")]
    pub workspace: Option<PathBuf>,

    #[arg(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    #[arg(long = "output", value_name = "DIR")]
    pub output: Option<PathBuf>,

    #[arg(long = "clean")]
    pub clean: bool,

    #[arg(long = "dry-run")]
    pub dry_run: bool,

    #[arg(long = "external")]
    pub external: bool,

    #[arg(long = "verbose")]
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub workspace_input: PathBuf,
    pub manifest_path: Option<PathBuf>,
    pub output_raw: PathBuf,
    pub clean: bool,
    pub dry_run: bool,
    pub external: bool,
    pub verbose: bool,
}

fn normalize_absolute(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        out
    }
}

fn resolve_output(output: Option<&Path>, cwd: &Path) -> PathBuf {
    match output {
        Some(p) if p.is_absolute() => normalize_absolute(p),
        Some(p) => normalize_absolute(&cwd.join(p)),
        None => normalize_absolute(&cwd.join("obsidian-graph")),
    }
}

pub fn resolve_config(args: &CliArgs, cwd: &Path) -> Result<Config, String> {
    if args.workspace.is_some() && args.manifest_path.is_some() {
        return Err("workspace and --manifest-path are mutually exclusive".to_string());
    }
    Ok(Config {
        workspace_input: args.workspace.clone().unwrap_or_else(|| PathBuf::from(".")),
        manifest_path: args.manifest_path.clone(),
        output_raw: resolve_output(args.output.as_deref(), cwd),
        clean: args.clean,
        dry_run: args.dry_run,
        external: args.external,
        verbose: args.verbose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_output_kept_normalized() {
        let args = CliArgs {
            workspace: None,
            manifest_path: None,
            output: Some(PathBuf::from("/v/./graph")),
            clean: false,
            dry_run: false,
            external: false,
            verbose: false,
        };
        let cfg = resolve_config(&args, Path::new("/work")).unwrap();
        assert_eq!(cfg.output_raw, PathBuf::from("/v/graph"));
    }
}
