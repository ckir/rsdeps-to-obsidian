use crate::{alpha, beta};
use crate::foo::{gamma, delta as delta_alias};
use crate::bar as bar_alias;
pub use crate::shared;
use self::sibling;
use super::parent_mod;
use serde::de::Deserializer;

mod inner {
    use crate::deep::thing;
    pub use super::up_thing;
}
