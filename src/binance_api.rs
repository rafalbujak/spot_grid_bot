use hmac::Mac;
use hmac::KeyInit;
use dotenvy::dotenv;
use hmac::Hmac;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

use crate::utils::{adjust_quantity, load_config};
use crate::database::{save_orders_to_db, display_orders};
const TRADING_FEE_RATE: f64 = 0.001; // 0.1% standardowa opłata Binance

/// Składa zlecenie kupna lub sprzedaży na Binance
pub(crate) async fn place_binance_order(
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
pub(crate) fn generate_signature(query: &str, secret_key: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret_key.as_bytes()).expect("HMAC can take key of any size");
    mac.update(query.as_bytes());
    hex::encode(mac.finalize().into_bytes())
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

pub(crate) async fn get_price(symbol: &str, client: &Client) -> Result<f64, reqwest::Error> {
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


/// Pobiera aktywne zamówienia z Binance
pub(crate) async fn show_binance_orders(db: &mut Connection) {
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
pub(crate) async fn get_binance_server_time() -> Result<u128, reqwest::Error> {
    let client = reqwest::Client::new();
    let url = "https://api.binance.com/api/v3/time";
    let response = client.get(url).send().await?.json::<serde_json::Value>().await?;
    Ok(response["serverTime"].as_u64().unwrap_or(0) as u128)
}





pub(crate) async fn get_lot_size(symbol: &str) -> Result<(f64, f64), String> {
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

pub(crate) async fn get_filled_sell_orders(db: &mut Connection) -> Vec<(String, f64, f64)> {
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
