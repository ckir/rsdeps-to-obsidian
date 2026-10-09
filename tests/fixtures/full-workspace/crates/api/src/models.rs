pub struct User {
    pub name: String,
}

pub struct Admin {
    pub id: u64,
}

use crate::routes;
use crate::services::Worker;

#[allow(dead_code)]
fn _touch() {
    let _ = Admin { id: 0 };
    let _ = routes::Router;
    let _ = Worker { queue: String::new() };
}
