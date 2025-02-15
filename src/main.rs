use hmac::KeyInit;
use tokio::time::{sleep, Duration};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection};
use std::env;
use std::fs;
use std::collections::HashMap;
use clap::{Command, Arg};
use std::io;
use futures::future::join_all;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use std::time::{SystemTime, UNIX_EPOCH};
use dotenvy::dotenv;
use serde_json::Value;

use serde_json::json;

/// Wysyła zlecenie na Binance i zwraca `orderId`


const TRADING_FEE_RATE: f64 = 0.001; // 0.1% standardowa opłata Binance

/// Składa zlecenie kupna lub sprzedaży na Binance
async fn place_binance_order(
    client: &Client,
    api_key: &str,
    secret_key: &str,
    symbol: &str,
    side: &str,
    price: f64,
    quantity: f64
) -> Result<u64, String> {
    let timestamp = get_binance_server_time().await.unwrap_or(0);
    let mut adjusted_quantity = quantity;

    // ⚠️ Uwzględnienie opłat Binance przy sprzedaży
    if side == "SELL" {
        adjusted_quantity *= (1.0 - TRADING_FEE_RATE);
    }

    // 🔄 Pobranie wymaganej wielkości lota (LOT_SIZE)
    let (min_qty, step_size) = get_lot_size(symbol).await.unwrap_or((0.01, 0.01));
    adjusted_quantity = adjust_quantity(adjusted_quantity, step_size);

    if adjusted_quantity < min_qty {
        println!(
            "⚠️ Skipping order for {} at {:.2}, below min LOT_SIZE ({:.5})",
            symbol, price, min_qty
        );
        return Err("Quantity below minimum LOT_SIZE".to_string());
    }

    // 🔄 Sprawdzenie dostępnego balansu przed sprzedażą
    if side == "SELL" {
        let base_asset = symbol.chars().take_while(|&c| c.is_alphabetic()).collect::<String>(); // np. "LTC" z "LTCUSDC"
        let available_balance = get_available_balance(&base_asset, api_key, secret_key).await.unwrap_or(0.0);

        if available_balance < adjusted_quantity {
            println!(
                "❌ Insufficient balance for selling {}: Available: {:.5}, Needed: {:.5}",
                base_asset, available_balance, adjusted_quantity
            );
            return Err("Insufficient balance for requested action".to_string());
        }
    }

    let query_string = format!(
        "symbol={}&side={}&type=LIMIT&timeInForce=GTC&quantity={:.6}&price={:.2}&timestamp={}",
        symbol, side, adjusted_quantity, price, timestamp
    );

    let signature = generate_signature(&query_string, secret_key);
    let url = format!(
        "https://api.binance.com/api/v3/order?{}&signature={}",
        query_string, signature
    );

    let mut headers = HeaderMap::new();
    headers.insert("X-MBX-APIKEY", HeaderValue::from_str(api_key).unwrap());
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    println!(
        "🛑 Attempting to place {} order on Binance:\n  Symbol: {}\n  Price: {:.8}\n  Quantity: {:.8}\n  Total Cost: {:.8}",
        side, symbol, price, adjusted_quantity, price * adjusted_quantity
    );

    let response = client.post(&url).headers(headers).send().await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let json_resp: Value = resp.json().await.unwrap();
            let order_id = json_resp["orderId"].as_u64().unwrap_or(0);
            println!("✅ Order placed on Binance: {} | Order ID: {}", symbol, order_id);
            Ok(order_id)
        }
        Ok(resp) => {
            let error_msg = resp.text().await.unwrap();
            println!("❌ Binance order failed: {}", error_msg);
            Err(error_msg)
        }
        Err(e) => {
            println!("❌ Request error: {}", e);
            Err(e.to_string())
        }
    }
}

async fn get_available_balance(asset: &str, api_key: &str, secret_key: &str) -> Result<f64, String> {
    let client = Client::new();
    let timestamp = get_binance_server_time().await.unwrap_or(0);
    let query_string = format!("timestamp={}", timestamp);
    let signature = generate_signature(&query_string, secret_key);

    let url = format!(
        "https://api.binance.com/api/v3/account?{}&signature={}",
        query_string, signature
    );

    let mut headers = HeaderMap::new();
    headers.insert("X-MBX-APIKEY", HeaderValue::from_str(api_key).unwrap());
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let response = client.get(&url).headers(headers).send().await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let json_resp: Value = resp.json().await.unwrap();
            if let Some(balances) = json_resp["balances"].as_array() {
                for balance in balances {
                    if balance["asset"] == asset {
                        let free_balance = balance["free"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);
                        return Ok(free_balance);
                    }
                }
            }
            Err("Asset not found in balance.".to_string())
        }
        Ok(resp) => Err(resp.text().await.unwrap_or_else(|_| "Unknown error".to_string())),
        Err(e) => Err(e.to_string()),
    }
}




/// Funkcja do generowania sygnatury HMAC-SHA256 dla API Binance
fn generate_signature(query: &str, secret_key: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret_key.as_bytes()).expect("HMAC can take key of any size");
    mac.update(query.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}



fn load_config(filename: &str) -> HashMap<String, String> {
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

#[derive(Debug, Serialize, Deserialize)]
struct BinanceTicker {
    symbol: String,
    price: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct OrderResponse {
    orderId: u64,
}

async fn get_price(symbol: &str, client: &Client) -> Result<f64, reqwest::Error> {
    let url = format!("https://api.binance.com/api/v3/ticker/price?symbol={}", symbol);
    let response: BinanceTicker = client.get(&url).send().await?.json().await?;
    Ok(response.price.parse().unwrap_or(0.0))
}

async fn get_min_notional(symbol: &str) -> Result<f64, reqwest::Error> {
    let url = "https://api.binance.com/api/v3/exchangeInfo";
    let client = reqwest::Client::new();

    let response = client.get(url).send().await?.json::<serde_json::Value>().await?;

    if let Some(symbols) = response["symbols"].as_array() {
        for s in symbols {
            if s["symbol"] == symbol {
                if let Some(filters) = s["filters"].as_array() {
                    for filter in filters {
                        if filter["filterType"] == "NOTIONAL" {
                            return Ok(filter["minNotional"].as_str().unwrap_or("10").parse().unwrap_or(10.0));
                        }
                    }
                }
            }
        }
    }
    Ok(10.0) // Domyślna wartość, jeśli API nie zwróci informacji
}

fn setup_db() -> Connection {
    let conn = Connection::open("trades.db").expect("Failed to open DB");

    conn.execute(
        "CREATE TABLE IF NOT EXISTS trades (
            id INTEGER PRIMARY KEY,
            symbol TEXT NOT NULL,
            price REAL NOT NULL,
            quantity REAL NOT NULL,
            timestamp TEXT NOT NULL,
            type TEXT NOT NULL,
            profit REAL,
            order_id INTEGER UNIQUE
        )",
        [],
    ).expect("Failed to create table");

    conn.execute(
        "CREATE TABLE IF NOT EXISTS capital (
            id INTEGER PRIMARY KEY,
            symbol TEXT NOT NULL,
            amount REAL NOT NULL,
            min_price REAL NOT NULL,
            max_price REAL NOT NULL,
            is_active INTEGER DEFAULT 05


        )",
        [],
    ).expect("Failed to create capital table");
    conn.execute("CREATE INDEX IF NOT EXISTS idx_symbol ON trades(symbol);", []).unwrap();
    conn.execute("CREATE INDEX IF NOT EXISTS idx_timestamp ON trades(timestamp);", []).unwrap();

    conn.execute(
        "CREATE TABLE IF NOT EXISTS orders (
            id INTEGER PRIMARY KEY,
            order_id INTEGER UNIQUE,
            symbol TEXT NOT NULL,
            price REAL NOT NULL,
            stop_price REAL NOT NULL,
            quantity REAL NOT NULL,
            type TEXT NOT NULL,
            status TEXT NOT NULL,
            timestamp TEXT NOT NULL
        )",
        [],
    ).expect("Failed to create orders table");

    conn
}

/// Zapisuje zlecenia do bazy danych
///
///
fn save_orders_to_db(db: &mut Connection, orders: &serde_json::Value) {
    let tx = db.transaction().expect("Failed to start transaction");

    // Lista ID zamówień z Binance API (do usuwania starych zamówień)
    let mut active_order_ids = Vec::new();

    if let Some(order_list) = orders.as_array() {
        for order in order_list {
            let order_id = order["orderId"].as_u64().unwrap_or(0);
            let symbol = order["symbol"].as_str().unwrap_or("N/A");
            let price = order["price"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);
            let stop_price = order["stopPrice"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);
            let quantity = order["origQty"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);
            let order_type = order["type"].as_str().unwrap_or("UNKNOWN");
            let status = order["status"].as_str().unwrap_or("UNKNOWN");
            let timestamp = order["time"].as_u64().unwrap_or_else(|| {
                eprintln!("❌ Błąd: Brak timestamp w zamówieniu: {:?}", order);
                0 // Wstawienie wartości domyślnej zamiast NULL
            });

            active_order_ids.push(order_id);

            tx.execute(
                "INSERT OR IGNORE INTO orders (order_id, symbol, price, stop_price, quantity, type, status, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime(?8 / 1000, 'unixepoch'))",
                params![order_id, symbol, price, stop_price, quantity, order_type, status, timestamp],
            ).expect("Failed to insert order");
        }
    }

    // Usuwanie zamówień, które już nie istnieją na Binance
    let active_order_ids_str = active_order_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    tx.execute(
        &format!("DELETE FROM orders WHERE order_id NOT IN ({})", active_order_ids_str),
        [],
    ).expect("Failed to delete old orders");

    tx.commit().expect("Failed to commit transaction");
}



fn get_user_input(prompt: &str) -> String {
    let mut input = String::new();
    println!("{}", prompt);
    io::stdin().read_line(&mut input).expect("Failed to read input");
    input.trim().to_string()
}


fn set_capital_for_pair(db: &mut Connection) {
    let symbol = get_user_input("Enter trading pair symbol (e.g., BTCUSDT):");
    let amount: f64 = get_user_input("Enter capital allocation for this pair:")
        .parse()
        .expect("Invalid capital amount");
    let min_price: f64 = get_user_input("Enter minimum price range:")
        .parse()
        .expect("Invalid min price");
    let max_price: f64 = get_user_input("Enter maximum price range:")
        .parse()
        .expect("Invalid max price");

    db.execute(
        "INSERT OR REPLACE INTO capital (symbol, amount, min_price, max_price)
         VALUES (?1, ?2, ?3, ?4)",
        params![symbol, amount, min_price, max_price],
    ).expect("Failed to set capital allocation for pair");

    println!("✅ Capital allocation for {} set to: ${:.2}, price range: {:.2} - {:.2}",
             symbol, amount, min_price, max_price);
}



fn show_capital_for_pairs(db: &Connection) {
    let mut stmt = db.prepare("SELECT symbol, amount FROM capital ORDER BY symbol ASC").expect("Failed to prepare statement");
    let capital_entries = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    }).expect("Failed to query capital entries");

    println!("\nCapital Allocations:");
    for entry in capital_entries {
        let (symbol, amount) = entry.expect("Failed to fetch capital entry");
        println!("Pair: {}, Capital: ${:.2}", symbol, amount);
    }
}

fn show_open_positions(db: &Connection) {
    let mut stmt = db.prepare("SELECT id, symbol, price, quantity, timestamp, type FROM trades ORDER BY timestamp DESC").expect("Failed to prepare statement");
    let positions = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, f64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?
        ))
    }).expect("Failed to query open positions");

    println!("\nOpen Positions:");
    for position in positions {
        let (id, symbol, price, quantity, timestamp, trade_type) = position.expect("Failed to fetch position");
        let trade_direction = if trade_type == "Buy" { "🔵 Buy" } else { "🔴 Sell" };
        println!("ID: {}, Symbol: {}, Price: {:.2}, Quantity: {:.2}, Timestamp: {}, Type: {}", id, symbol, price, quantity, timestamp, trade_direction);
    }
}

fn manage_active_orders(db: &Connection) {
    let mut stmt = db.prepare("SELECT COUNT(*) FROM trades WHERE type IN ('Buy', 'Sell')").expect("Failed to prepare statement");
    let active_orders: i32 = stmt.query_row([], |row| row.get(0)).unwrap_or(0);

    if active_orders >= 5 {
        println!("Max 5 active orders reached. No new orders will be placed until existing ones are closed.");
    } else {
        println!("{} active orders. New orders can be placed.", active_orders);
    }
}

fn display_orders(db: &Connection) {
    let mut stmt = db.prepare("SELECT symbol, price, stop_price, quantity, type, status, timestamp FROM orders ORDER BY timestamp DESC")
        .expect("Failed to prepare statement");

    let orders = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?, // symbol
            row.get::<_, f64>(1)?,    // price
            row.get::<_, f64>(2)?,    // stop_price
            row.get::<_, f64>(3)?,    // quantity
            row.get::<_, String>(4)?, // type
            row.get::<_, String>(5)?, // status
            row.get::<_, String>(6)?  // timestamp
        ))
    }).expect("Failed to query orders");

    println!("\n📊 **Aktywne zlecenia Binance:**\n");
    for order in orders {
        let (symbol, price, stop_price, quantity, order_type, status, timestamp) = order.expect("Failed to fetch order");
        println!(
            "🔹 **Para:** {} | 🏷️ **Typ:** {} | 📌 **Status:** {}\n   💰 **Cena:** {:.2} | ⛔ **Stop:** {:.2} | 🔢 **Ilość:** {:.5} | 📅 **Czas:** {}\n",
            symbol, order_type, status, price, stop_price, quantity, timestamp
        );
    }
}

/// Pobiera aktywne zamówienia z Binance
async fn show_binance_orders(db: &mut Connection) {
    dotenv().ok();
    let config = load_config("config.txt");
    let api_key = config.get("BINANCE_API_KEY").expect("Missing API key");
    let secret_key = config.get("BINANCE_SECRET_KEY").expect("Missing secret key");
    let client = reqwest::Client::new();

    let timestamp = get_binance_server_time().await.unwrap_or(0);
    let query_string = format!("timestamp={}", timestamp);
    let signature = generate_signature(&query_string, &secret_key);

    let url = format!("https://api.binance.com/api/v3/openOrders?{}&signature={}", query_string, signature);

    let mut headers = HeaderMap::new();
    headers.insert("X-MBX-APIKEY", HeaderValue::from_str(&api_key).unwrap());
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let response = client.get(&url).headers(headers).send().await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let orders: serde_json::Value = resp.json().await.unwrap();
            save_orders_to_db(db, &orders);  // ✅ Poprawne użycie `db`
            display_orders(db);  // ✅ Poprawne użycie `db`
        }
        Ok(resp) => {
            println!("Failed to fetch orders: {}", resp.text().await.unwrap());
        }
        Err(e) => {
            println!("Request error: {}", e);
        }
    }
}

///  synchrnizacja czasu
async fn get_binance_server_time() -> Result<u128, reqwest::Error> {
    let client = reqwest::Client::new();
    let url = "https://api.binance.com/api/v3/time";
    let response = client.get(url).send().await?.json::<serde_json::Value>().await?;
    Ok(response["serverTime"].as_u64().unwrap_or(0) as u128)
}

/// Pobiera ostatnie transakcje użytkownika (wykonane zlecenia)
async fn show_live_execution() {
    dotenv().ok();

    let config = load_config("config.txt");
    let api_key = config.get("BINANCE_API_KEY").expect("Missing API key");
    let secret_key = config.get("BINANCE_SECRET_KEY").expect("Missing secret key");

    let client = reqwest::Client::new();

    let symbol = "BTCUSDT"; // Można zmienić na inny symbol

    //let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let timestamp = get_binance_server_time().await.unwrap_or(0);

    let query_string = format!("symbol={}&timestamp={}", symbol, timestamp);
    let signature = generate_signature(&query_string, &secret_key);

    let url = format!("https://api.binance.com/api/v3/myTrades?{}&signature={}", query_string, signature);

    let mut headers = HeaderMap::new();
    headers.insert("X-MBX-APIKEY", HeaderValue::from_str(&api_key).unwrap());
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let response = client.get(&url).headers(headers).send().await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let trades: serde_json::Value = resp.json().await.unwrap();
            println!("Recent Trades: {}", serde_json::to_string_pretty(&trades).unwrap());
        }
        Ok(resp) => {
            println!("Failed to fetch trades: {}", resp.text().await.unwrap());
        }
        Err(e) => {
            println!("Request error: {}", e);
        }
    }
}

fn show_remaining_capital(db: &Connection) {
    // Pobranie dostępnych par walutowych
    let mut stmt = db.prepare("SELECT id, symbol FROM capital ORDER BY symbol ASC")
        .expect("Failed to prepare statement");

    let symbols: Vec<(i32, String)> = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })
        .expect("Failed to query capital pairs")
        .filter_map(Result::ok)
        .collect();

    if symbols.is_empty() {
        println!("❌ No trading pairs available. Please set capital for a pair first.");
        return;
    }

    // Wyświetlenie listy par walutowych
    println!("\n📊 **Available trading pairs:**");
    for (id, symbol) in &symbols {
        println!("{}. {}", id, symbol);
    }

    // Pobranie wyboru od użytkownika
    let choice: i32 = get_user_input("Select a trading pair by ID:")
        .parse()
        .unwrap_or(0);

    if !symbols.iter().any(|(id, _)| *id == choice) {
        println!("❌ Invalid selection.");
        return;
    }

    // Pobranie symbolu dla wybranego ID
    let symbol = symbols.iter().find(|(id, _)| *id == choice).unwrap().1.clone();

    // Sprawdzenie, czy grid trading został już uruchomiony (czy są transakcje dla pary)
    let has_trades: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM trades WHERE symbol = ?1)",
        params![symbol],
        |row| row.get(0)
    ).unwrap_or(false);

    let remaining_capital: f64;

    if has_trades {
        // Pobranie pozostałego kapitału na kolejne zlecenia
        remaining_capital = db.query_row(
            "SELECT SUM(quantity) FROM trades WHERE symbol = ?1",
            params![symbol],
            |row| row.get(0)
        ).unwrap_or(0.0);

        println!("\n🔹 **Pair:** {} | 💰 **Remaining capital for new orders:** ${:.2}", symbol, remaining_capital);
    } else {
        // Pobranie pełnego dostępnego kapitału (gdy grid nie został uruchomiony)
        remaining_capital = db.query_row(
            "SELECT amount FROM capital WHERE symbol = ?1",
            params![symbol],
            |row| row.get(0)
        ).unwrap_or(0.0);

        println!("\n🔹 **Pair:** {} | 💰 **Available capital (Grid not started yet):** ${:.2}", symbol, remaining_capital);
    }
}

async fn get_lot_size(symbol: &str) -> Result<(f64, f64), String> {
    let client = Client::new();
    let url = "https://api.binance.com/api/v3/exchangeInfo";

    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    let json: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;

    if let Some(symbols) = json["symbols"].as_array() {
        for s in symbols {
            if s["symbol"] == symbol {
                if let Some(filters) = s["filters"].as_array() {
                    for filter in filters {
                        if filter["filterType"] == "LOT_SIZE" {
                            let min_qty = filter["minQty"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);
                            let step_size = filter["stepSize"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);
                            return Ok((min_qty, step_size));
                        }
                    }
                }
            }
        }
    }

    Err("Could not retrieve LOT_SIZE".to_string())
}


fn has_open_positions(db: &Connection, symbol: &str) -> bool {
    let count: i32 = db.query_row(
        "SELECT COUNT(*) FROM trades WHERE symbol = ?1",
        params![symbol],
        |row| row.get(0),
    ).unwrap_or(0);
    count > 0
}


async fn get_account_balance(asset: &str, api_key: &str, secret_key: &str) -> Result<f64, String> {
    let client = Client::new();
    let timestamp = get_binance_server_time().await.unwrap_or(0);
    let query_string = format!("timestamp={}", timestamp);
    let signature = generate_signature(&query_string, secret_key);

    let url = format!(
        "https://api.binance.com/api/v3/account?{}&signature={}",
        query_string, signature
    );

    let mut headers = HeaderMap::new();
    headers.insert("X-MBX-APIKEY", HeaderValue::from_str(api_key).unwrap());
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let response = client.get(&url).headers(headers).send().await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let json_resp: serde_json::Value = resp.json().await.unwrap();
            if let Some(balances) = json_resp["balances"].as_array() {
                for balance in balances {
                    if balance["asset"] == asset {
                        let free_balance = balance["free"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);
                        let locked_balance = balance["locked"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);

                        println!(
                            "🔹 {} Balance: Free = {:.2}, Locked = {:.2}",
                            asset, free_balance, locked_balance
                        );

                        return Ok(free_balance);
                    }
                }
            }
            Err("Asset not found in balance.".to_string())
        }
        Ok(resp) => Err(resp.text().await.unwrap_or_else(|_| "Unknown error".to_string())),
        Err(e) => Err(e.to_string()),
    }
}

fn adjust_quantity(quantity: f64, step_size: f64) -> f64 {
    (quantity / step_size).trunc() * step_size
}




async fn get_filled_sell_orders(db: &mut Connection) -> Vec<(String, f64, f64)> {
    dotenv().ok();
    let config = load_config("config.txt");
    let api_key = config.get("BINANCE_API_KEY").expect("Missing API key");
    let secret_key = config.get("BINANCE_SECRET_KEY").expect("Missing secret key");
    let client = reqwest::Client::new();

    let timestamp = get_binance_server_time().await.unwrap_or(0);
    let query_string = format!("timestamp={}", timestamp);
    let signature = generate_signature(&query_string, &secret_key);

    let url = format!("https://api.binance.com/api/v3/openOrders?{}&signature={}", query_string, signature);

    let mut headers = HeaderMap::new();
    headers.insert("X-MBX-APIKEY", HeaderValue::from_str(&api_key).unwrap());
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let response = client.get(&url).headers(headers).send().await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let orders: serde_json::Value = resp.json().await.unwrap();
            let mut filled_sell_orders = Vec::new();

            if let Some(order_list) = orders.as_array() {
                for order in order_list {
                    let status = order["status"].as_str().unwrap_or("UNKNOWN");
                    let order_type = order["side"].as_str().unwrap_or("UNKNOWN");
                    let symbol = order["symbol"].as_str().unwrap_or("UNKNOWN").to_string();
                    let price = order["price"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);
                    let quantity = order["origQty"].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(0.0);

                    if status == "FILLED" && order_type == "SELL" {
                        filled_sell_orders.push((symbol, price, quantity));
                    }
                }
            }

            return filled_sell_orders;
        }
        Ok(resp) => {
            println!("Failed to fetch orders: {}", resp.text().await.unwrap());
        }
        Err(e) => {
            println!("Request error: {}", e);
        }
    }

    Vec::new()
}

async fn monitor_and_reinvest(db: &mut Connection) {
    loop {
        let filled_orders = get_filled_sell_orders(db).await;

        for (symbol, sell_price, quantity) in filled_orders {
            let reinvest_price = sell_price * 0.95; // -5% od ceny sprzedaży
            let reinvest_quantity = quantity * 1.0; // reinwestowanie 105% wartości

            let (min_qty, step_size) = get_lot_size(&symbol).await.unwrap_or((0.01, 0.01));
            let adjusted_quantity = adjust_quantity(reinvest_quantity, step_size);

            if adjusted_quantity < min_qty {
                println!(
                    "⚠️ Skipping reinvestment order for {} at {:.2}, below min LOT_SIZE ({:.5})",
                    symbol, reinvest_price, min_qty
                );
                continue;
            }

            println!(
                "🔄 Reinvesting for {} | Buy at {:.2}, Quantity: {:.5}",
                symbol, reinvest_price, adjusted_quantity
            );

            let buy_order_id = place_binance_order(
                &Client::new(),
                &load_config("config.txt").get("BINANCE_API_KEY").unwrap(),
                &load_config("config.txt").get("BINANCE_SECRET_KEY").unwrap(),
                &symbol,
                "BUY",
                reinvest_price,
                adjusted_quantity
            ).await.unwrap_or(0);

            if buy_order_id > 0 {
                db.execute(
                    "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit, order_id)
                     VALUES (?1, ?2, ?3, datetime('now'), 'Buy', NULL, ?4)",
                    params![symbol, reinvest_price, adjusted_quantity, buy_order_id],
                ).expect("Failed to insert reinvestment buy order");

                db.execute(
                    "UPDATE capital SET amount = amount + (?1 * ?2) WHERE symbol = ?3",
                    params![sell_price, quantity, symbol],
                ).expect("Failed to update capital after reinvestment");
            }
        }

        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn execute_grid_trade(db: &mut Connection) {
    // 📌 Pobranie dostępnych par walutowych
    let symbols: Vec<(String, i32)> = {
        let mut stmt = db.prepare("SELECT symbol, is_active FROM capital ORDER BY symbol ASC")
            .expect("Failed to prepare statement");

        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("Failed to query capital pairs")
            .filter_map(Result::ok)
            .collect()
    };

    if symbols.is_empty() {
        println!("❌ No trading pairs available. Please set capital for a pair first.");
        return;
    }

    println!("\nAvailable trading pairs:");
    for (index, (symbol, is_active)) in symbols.iter().enumerate() {
        let status = if *is_active == 1 { "🟢 Active" } else { "🔴 Inactive" };
        println!("{}. {} {}", index + 1, symbol, status);
    }

    let choice: usize = get_user_input("Select a trading pair by number:")
        .parse()
        .unwrap_or(0);

    if choice == 0 || choice > symbols.len() {
        println!("❌ Invalid selection.");
        return;
    }

    let (symbol, is_active) = &symbols[choice - 1];

    if *is_active == 1 {
        println!("❌ Trading bot for {} is already running.", symbol);
        return;
    }

    let (mut capital, min_price, max_price): (f64, f64, f64) = db.query_row(
        "SELECT amount, min_price, max_price FROM capital WHERE symbol = ?1",
        params![symbol],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    ).unwrap_or((0.0, 0.0, 0.0));

    if capital < 10.0 {
        println!("❌ Insufficient capital for trading this pair.");
        return;
    }

    let api_key = load_config("config.txt").get("BINANCE_API_KEY").unwrap().clone();
    let secret_key = load_config("config.txt").get("BINANCE_SECRET_KEY").unwrap().clone();
    let client = Client::new();

    let current_price = match get_price(symbol, &client).await {
        Ok(price) => price,
        Err(_) => {
            println!("❌ Failed to fetch price for {}", symbol);
            return;
        }
    };

    // 📌 Pobranie wymagań `LOT_SIZE`
    let (min_qty, step_size) = get_lot_size(symbol).await.unwrap_or((0.01, 0.01));

    // 📌 Kupno 3 pozycji od razu po aktualnej cenie
    let order_value = capital * 0.1; // 10% kapitału na każde z 3 zleceń
    let sell_levels = [0.05, 0.10, 0.15]; // +5%, +10%, +15% od ceny zakupu

    println!(
        "✅ Buying 3 initial positions for {} at {:.2} | Order Value: {:.2} USDC each",
        symbol, current_price, order_value
    );

    for (i, &sell_offset) in sell_levels.iter().enumerate() {
        let mut buy_quantity = order_value / current_price;
        buy_quantity = adjust_quantity(buy_quantity, step_size);

        if buy_quantity < min_qty {
            println!("⚠️ Skipping buy order at {:.2}, below minimum LOT_SIZE ({:.5})", current_price, min_qty);
            continue;
        }

        let buy_order_id = place_binance_order(
            &client, &api_key, &secret_key, symbol, "BUY", current_price, buy_quantity
        ).await.unwrap_or(0);

        if buy_order_id > 0 {
            db.execute(
                "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit, order_id)
                 VALUES (?1, ?2, ?3, datetime('now'), 'Buy', NULL, ?4)",
                params![symbol, current_price, buy_quantity, buy_order_id],
            ).expect("Failed to insert buy order");

            // 📌 Automatyczna sprzedaż
            let sell_price = current_price * (1.0 + sell_offset);
            let sell_quantity = buy_quantity;

            let sell_order_id = place_binance_order(
                &client, &api_key, &secret_key, symbol, "SELL", sell_price, sell_quantity
            ).await.unwrap_or(0);

            if sell_order_id > 0 {
                db.execute(
                    "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit, order_id)
                     VALUES (?1, ?2, ?3, datetime('now'), 'Sell', NULL, ?4)",
                    params![symbol, sell_price, sell_quantity, sell_order_id],
                ).expect("Failed to insert sell order");
            }
        }
    }

    // 📌 Ustawienie 2 poziomów kupna poniżej aktualnej ceny
    let buy_grid_levels = [-0.05, -0.10]; // -5%, -10% poniżej aktualnej ceny

    for &buy_offset in &buy_grid_levels {
        let buy_price = current_price * (1.0 + buy_offset);
        let mut buy_quantity = order_value / buy_price;
        buy_quantity = adjust_quantity(buy_quantity, step_size);

        if buy_quantity < min_qty {
            println!("⚠️ Skipping grid buy order at {:.2}, below minimum LOT_SIZE ({:.5})", buy_price, min_qty);
            continue;
        }

        place_binance_order(&client, &api_key, &secret_key, symbol, "BUY", buy_price, buy_quantity).await.unwrap_or(0);
    }

    db.execute(
        "UPDATE capital SET is_active = 1 WHERE symbol = ?1",
        params![symbol],
    ).expect("Failed to update trading bot status");

    println!("✅ Trading bot for {} started successfully!", symbol);
}











/*async fn execute_grid_trade(db: &Connection) {

    let symbol = get_user_input("Enter trading pair symbol to execute grid trade (e.g., BTCUSDT):");
    let min_price: f64 = get_user_input("Enter minimum price range:").parse().expect("Invalid min price");
    let max_price: f64 = get_user_input("Enter maximum price range:").parse().expect("Invalid max price");
    let capital: f64 = db.query_row(
        "SELECT amount FROM capital WHERE symbol = ?1",
        params![symbol],
        |row| row.get(0),
    ).unwrap_or(0.0);

    if capital < 10.0 {
        println!("Insufficient capital for trading this pair.");
        return;
    }

    let grid_step = 0.03;
    let order_size = 2.0;
    let mut buy_orders = 0;
    let mut sell_orders = 0;
    let client = Client::new();
    let current_price = match get_price(&symbol, &client).await {
        Ok(price) => price,
        Err(_) => {
            println!("Failed to fetch price for {}", symbol);
            return;
        }
    };

    if !has_open_positions(db, &symbol) {
        println!("No open positions found for {}. Initializing starting grid with 3 buy and 3 sell orders.", symbol);
        println!("Initializing grid for {} at price {:.2}", symbol, current_price);

        for i in 0..3 {
            let buy_price = current_price * (1.0 - (grid_step * (i + 1) as f64));
            let sell_price = current_price * (1.0 + (grid_step * (i + 1) as f64));

            println!("Placing Buy order at {:.2} for ${:.2}", buy_price, order_size);
            db.execute(
                "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit) VALUES (?1, ?2, ?3, datetime('now'), 'Buy', NULL)",
                params![symbol, buy_price, order_size],
            ).expect("Failed to insert buy order");

            println!("Placing Sell order at {:.2} for ${:.2}", sell_price, order_size);
            db.execute(
                "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit) VALUES (?1, ?2, ?3, datetime('now'), 'Sell', NULL)",
                params![symbol, sell_price, order_size],
            ).expect("Failed to insert sell order");
        }
    } else {
        println!("Executing grid strategy for {} in price range {} - {}", symbol, min_price, max_price);
        let mut price = min_price;
        while price <= max_price && (buy_orders + sell_orders) < 5 {
            if buy_orders < 3 {
                println!("Placing Buy order at {:.2} for ${:.2}", price, order_size);
                db.execute(
                    "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit) VALUES (?1, ?2, ?3, datetime('now'), 'Buy', NULL)",
                    params![symbol, price, order_size],
                ).expect("Failed to insert buy order");
                buy_orders += 1;
            }
            if sell_orders < 2 {
                let sell_price = price * (1.0 + grid_step);
                println!("Placing Sell order at {:.2} for ${:.2}", sell_price, order_size);
                db.execute(
                    "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit) VALUES (?1, ?2, ?3, datetime('now'), 'Sell', NULL)",
                    params![symbol, sell_price, order_size],
                ).expect("Failed to insert sell order");
                sell_orders += 1;
            }
            price += price * grid_step;
        }
    }
}

*/
async fn show_menu(db: &mut Connection) {
    loop {
        println!("\nMenu:");
        println!("1. View open positions");
        println!("2. View orders placed on Binance");
        println!("3. View live order execution");
        println!("4. View active orders management");
        println!("5. View remaining capital");
        println!("6. Set capital for a trading pair");
        println!("7. View capital allocation per pair");
        println!("8. Execute grid trade for a pair");
        println!("9. Exit");

        let choice: String = get_user_input("Select an option:");
        match choice.as_str() {
            "1" => show_open_positions(db),
            "2" => show_binance_orders(db).await,
            "3" => show_live_execution().await,
            "4" => manage_active_orders(db),
            "5" => show_remaining_capital(db),
            "6" => set_capital_for_pair(db),
            "7" => show_capital_for_pairs(db),
            "8" => execute_grid_trade(db).await,
            "9" => break,
            _ => println!("Invalid option. Please try again."),
        }
    }
}

use std::sync::{Arc, Mutex};

use tokio::task;

#[tokio::main]
async fn main() {
    let db = Arc::new(Mutex::new(setup_db()));

    // 🚀 Uruchomienie reinwestowania w osobnym wątku
    let db_clone = Arc::clone(&db);
    task::spawn_blocking(move || {
        let mut db = db_clone.lock().expect("Failed to acquire DB lock");
        monitor_and_reinvest(&mut db);
    });

    let mut db_lock = db.lock().expect("Failed to acquire DB lock");
    show_menu(&mut db_lock).await;
}

