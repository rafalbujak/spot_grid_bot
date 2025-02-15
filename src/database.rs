use dotenvy::dotenv;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use rusqlite::{params, Connection};
use crate::binance_api::{generate_signature, get_binance_server_time};
use crate::utils::{load_config, get_user_input};

pub(crate) fn setup_db() -> Connection {
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
pub(crate) fn save_orders_to_db(db: &mut Connection, orders: &serde_json::Value) {
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


pub(crate) fn set_capital_for_pair(db: &mut Connection) {
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

pub(crate) fn show_capital_for_pairs(db: &Connection) {
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

pub(crate) fn show_open_positions(db: &Connection) {
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

pub(crate) fn manage_active_orders(db: &Connection) {
    let mut stmt = db.prepare("SELECT COUNT(*) FROM trades WHERE type IN ('Buy', 'Sell')").expect("Failed to prepare statement");
    let active_orders: i32 = stmt.query_row([], |row| row.get(0)).unwrap_or(0);

    if active_orders >= 5 {
        println!("Max 5 active orders reached. No new orders will be placed until existing ones are closed.");
    } else {
        println!("{} active orders. New orders can be placed.", active_orders);
    }
}

pub(crate) fn display_orders(db: &Connection) {
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
