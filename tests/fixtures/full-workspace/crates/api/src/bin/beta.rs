mod shared;

use crate::shared;
use serde::Serialize;

fn main() {
    let _ = shared::Help;
    let _: Option<&dyn Serialize> = None;
}
