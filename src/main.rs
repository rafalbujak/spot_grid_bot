mod binance_api;
mod database;
mod grid_trading;
mod utils;
mod menu;

use std::sync::{Arc, Mutex};
use tokio::task;
use rusqlite::Connection;
use crate::database::setup_db;
use crate::menu::show_menu;
use crate::grid_trading::monitor_and_reinvest;

#[tokio::main]
async fn main() {
    let db = Arc::new(Mutex::new(setup_db()));

    let db_clone = Arc::clone(&db);
    task::spawn_blocking(move || {
        let mut db = db_clone.lock().expect("Failed to acquire DB lock");
        tokio::runtime::Handle::current().block_on(monitor_and_reinvest(&mut db));
    });

    let mut db_lock = db.lock().expect("Failed to acquire DB lock");
    show_menu(&mut db_lock).await;
}

