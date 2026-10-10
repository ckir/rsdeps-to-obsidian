use clap::Parser;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Lang {
    Auto,
    Rust,
    Python,
    Typescript,
    Csharp,
}

impl std::fmt::Display for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Lang::Auto => write!(f, "auto"),
            Lang::Rust => write!(f, "rust"),
            Lang::Python => write!(f, "python"),
            Lang::Typescript => write!(f, "typescript"),
            Lang::Csharp => write!(f, "csharp"),
        }
    }
}

impl Lang {
    /// Lowercase language name for diagnostics (`auto` never reaches output:
    /// `run()` resolves it before rendering).
    pub fn name(self) -> &'static str {
        match self {
            Lang::Auto => "auto",
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::Typescript => "typescript",
            Lang::Csharp => "csharp",
        }
    }
}

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

    /// Language backend: an explicit value overrides autodetect silently;
    /// `auto` (default) scans the workspace for project files.
    #[arg(long, value_enum, default_value_t = Lang::Auto)]
    pub lang: Lang,
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
    pub lang: Lang,
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
        lang: args.lang,
    })
}

/// Project files that drive language autodetect, in priority order:
/// `Cargo.toml` > `pyproject.toml`/`setup.py` > `package.json`.
const PROJECT_FILES: [&str; 4] = ["Cargo.toml", "pyproject.toml", "setup.py", "package.json"];

/// Project files present directly under `dir`, in [`PROJECT_FILES`] priority
/// order. A directory entry that cannot be read (or `dir` itself missing)
/// simply yields no entry; detection never fails.
pub(crate) fn project_files_present(dir: &Path) -> Vec<&'static str> {
    PROJECT_FILES
        .iter()
        .copied()
        .filter(|name| dir.join(name).is_file())
        .collect()
}

/// Autodetect the language backend for `dir`: first present project file in
/// priority order wins (`pyproject.toml` and `setup.py` both mean Python);
/// a directory with no project file defaults to Rust.
pub fn detect_language(dir: &Path) -> Lang {
    match project_files_present(dir).as_slice() {
        [] => Lang::Rust,
        [first, ..] => match *first {
            "Cargo.toml" => Lang::Rust,
            "pyproject.toml" | "setup.py" => Lang::Python,
            _ => Lang::Typescript,
        },
    }
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
            lang: Lang::Auto,
        };
        let cfg = resolve_config(&args, Path::new("/work")).unwrap();
        assert_eq!(cfg.output_raw, PathBuf::from("/v/graph"));
    }
}
