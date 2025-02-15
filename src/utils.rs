use std::collections::HashMap;
use std::{fs, io};
use rusqlite::{params, Connection};

pub(crate) fn get_user_input(prompt: &str) -> String {
    let mut input = String::new();
    println!("{}", prompt);
    io::stdin().read_line(&mut input).expect("Failed to read input");
    input.trim().to_string()
}

pub(crate) fn adjust_quantity(quantity: f64, step_size: f64) -> f64 {
    (quantity / step_size).trunc() * step_size
}

pub(crate) fn load_config(filename: &str) -> HashMap<String, String> {
    let mut config = HashMap::new();
    if let Ok(contents) = fs::read_to_string(filename) {
        for line in contents.lines() {
            if let Some((key, value)) = line.split_once('=') {
                config.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }
    config
}

fn has_open_positions(db: &Connection, symbol: &str) -> bool {
    let count: i32 = db.query_row(
        "SELECT COUNT(*) FROM trades WHERE symbol = ?1",
        params![symbol],
        |row| row.get(0),
    ).unwrap_or(0);
    count > 0
}

