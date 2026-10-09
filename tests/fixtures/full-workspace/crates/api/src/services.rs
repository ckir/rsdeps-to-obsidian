pub mod deep;

pub struct Worker {
    pub queue: String,
}

use crate::services::deep;
use crate::services::deep::Job;

#[allow(dead_code)]
fn _touch() {
    let _ = deep::Job { name: String::new() };
    let _ = Job { name: String::new() };
}
