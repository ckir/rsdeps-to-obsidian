pub struct Router;

use crate::models;
use crate::models::User;
use super::services;

#[allow(dead_code)]
fn _touch() {
    let _ = User { name: String::new() };
    let _ = services::Worker { queue: String::new() };
    let _ = models::Admin { id: 0 };
}
