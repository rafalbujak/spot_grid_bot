use dotenvy::dotenv;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use rusqlite::{params, Connection};
use crate::grid_trading::execute_grid_trade;
use crate::binance_api::{generate_signature, get_binance_server_time, show_binance_orders};
use crate::utils::{get_user_input, load_config};
use crate::database::{manage_active_orders, set_capital_for_pair,
                      show_capital_for_pairs, show_open_positions,
                      };

pub(crate) async fn show_menu(db: &mut Connection) {
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
