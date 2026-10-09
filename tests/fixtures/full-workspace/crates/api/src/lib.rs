pub mod models;
pub mod routes;
pub mod services;
#[path = "special/renamed.rs"]
pub mod renamed;
pub mod qux;
pub mod con;
#[cfg(feature = "telemetry")]
pub mod gated;

pub mod inline_parent {
    pub mod inline_child {}
    use crate::models;
}

use crate::models;
pub use crate::models;
use crate::models::User;
use crate::routes as app_routes;
use crate::routes::Router;
use crate::{models as _, routes as _};
use crate::services::{deep, Worker};
use core::models;
use serde::Serialize;

#[cfg(unix)]
use crate::qux;

#[allow(unused_imports)]
use self::services::Worker as ServiceWorker;

#[allow(dead_code)]
fn _keep_aliases() {
    let _ = app_routes::Router;
    let _ = User { name: String::new() };
    let _ = Router;
    let _ = ServiceWorker { queue: String::new() };
    let _: Option<&dyn Serialize> = None;
}
