use crate::ids::{ExternalCrateId, ModuleId, PackageId, TargetId};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSpan {
    pub file: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Lib,
    Bin,
}

impl TargetKind {
    pub fn kind_token(&self) -> &'static str {
        match self {
            TargetKind::Lib => "lib",
            TargetKind::Bin => "bin",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub id: PackageId,
    pub name: String,
    pub manifest_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub id: TargetId,
    pub package_id: PackageId,
    pub name: String,
    pub kind: TargetKind,
    pub root_source: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub id: ModuleId,
    pub target_id: TargetId,
    pub name: String,
    pub qualified_name: String,
    pub source_path: Option<PathBuf>,
    pub source_span: Option<SourceSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DependencyTarget {
    Internal(ModuleId),
    External(ExternalCrateId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    Module,
    Use,
    ReExport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub from: ModuleId,
    pub to: DependencyTarget,
    pub kind: DependencyKind,
    pub source_span: Option<SourceSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub source_span: Option<SourceSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WorkspaceGraph {
    pub packages: Vec<Package>,
    pub targets: Vec<Target>,
    pub modules: Vec<Module>,
    pub dependencies: Vec<Dependency>,
    pub diagnostics: Vec<Diagnostic>,
}
