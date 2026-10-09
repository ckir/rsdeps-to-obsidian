pub struct Job {
    pub name: String,
}

use crate::models;

#[allow(dead_code)]
fn _touch() {
    let _ = models::User { name: String::new() };
}
