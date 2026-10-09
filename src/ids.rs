#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalCrateId(pub String);

pub fn make_target_id(package: &str, kind_token: &str, target: &str) -> Result<TargetId, String> {
    if kind_token != "lib" && kind_token != "bin" {
        return Err(format!("invalid kind_token: {kind_token}"));
    }
    for component in [package, kind_token, target] {
        if component.contains("::") {
            return Err(format!("component contains '::': {component}"));
        }
    }
    Ok(TargetId(format!("{package}::{kind_token}::{target}")))
}

pub fn make_module_id(package: &str, target: &str, path: &[String]) -> ModuleId {
    ModuleId(format!("{}::{}::{}", package, target, path.join("::")))
}

pub fn make_external_id(name: &str, version: &str) -> ExternalCrateId {
    ExternalCrateId(format!("{name}@{version}"))
}
