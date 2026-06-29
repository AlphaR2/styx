#![allow(dead_code, unused_imports)]
mod components;
mod app;
mod api;
mod pages;
mod utils;

use leptos::prelude::*;
use app::App;

fn main() {
    mount_to_body(App)
}
