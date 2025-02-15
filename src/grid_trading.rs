use std::time::Duration;
use dotenvy::dotenv;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use rusqlite::{params, Connection};
use crate::binance_api::{ get_filled_sell_orders, get_lot_size, get_price, place_binance_order,get_available_balance, extract_base_asset};
use crate::utils::{get_user_input, load_config, adjust_quantity};


pub(crate) async fn execute_grid_trade(db: &mut Connection) {
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

    // 📌 Pobieramy poprawny symbol bazowy np. "LTC" zamiast "LTCUSDC"
    let base_asset = extract_base_asset(symbol);
    println!("🔍 Final extracted base asset: {}", base_asset);

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

    let (min_qty, step_size) = get_lot_size(symbol).await.unwrap_or((0.01, 0.01));

    let order_value = 10.0; // Stała wartość dla każdej pozycji
    let sell_levels = [0.05, 0.10, 0.15]; // +5%, +10%, +15%
    let buy_levels = [-0.05, -0.10]; // -5%, -10%

    println!("✅ Buying 3 initial positions for {}", symbol);

    let mut initial_orders = Vec::new();

    for _ in 0..3 {
        let mut buy_quantity = order_value / current_price;
        buy_quantity = adjust_quantity(buy_quantity, step_size);

        let is_initial_buy = true;

        match place_binance_order(
            &client, &api_key, &secret_key, symbol, "BUY", current_price, buy_quantity, is_initial_buy
        ).await {
            Ok(order_id) => initial_orders.push((buy_quantity, order_id)),
            Err(e) => println!("❌ Failed to place initial buy order: {}", e),
        }
    }

    if initial_orders.len() == 3 {
        println!("✅ 3 initial positions bought. Waiting for balance update...");

        let mut retries = 5;
        let mut available_balance = 0.0;

        while retries > 0 {
            sleep(Duration::from_secs(2)).await;
            available_balance = get_available_balance(&base_asset, &api_key, &secret_key).await.unwrap_or(0.0);

            println!("🔄 Checking balance for {}... Available: {:.5}", base_asset, available_balance);

            if available_balance >= 0.075 { // Jeśli mamy wystarczająco LTC, wychodzimy z pętli
                break;
            }
            retries -= 1;
        }

        if available_balance < 0.075 {
            println!("❌ Error: Balance is still not available. Cannot place sell orders.");
            return;
        }

        println!("✅ Balance updated! Placing sell orders...");

        for (i, &(buy_quantity, _order_id)) in initial_orders.iter().enumerate() {
            let sell_price = current_price * (1.0 + sell_levels[i]);
            println!("🔄 Attempting to sell {:.5} {} at {:.2} USDT", buy_quantity, base_asset, sell_price);

            sleep(Duration::from_secs(2)).await;

            place_binance_order(
                &client, &api_key, &secret_key, symbol, "SELL", sell_price, buy_quantity, false
            ).await.unwrap_or(0);
        }

        // 📌 NOWE: Dodajemy 2 dodatkowe zlecenia kupna -5% i -10%
        println!("✅ Placing additional buy orders at -5% and -10%...");

        for &buy_offset in &buy_levels {
            let buy_price = current_price * (1.0 + buy_offset);
            let mut buy_quantity = order_value / buy_price;
            buy_quantity = adjust_quantity(buy_quantity, step_size);

            if buy_quantity < min_qty {
                println!(
                    "⚠️ Skipping buy order at {:.2}, below minimum LOT_SIZE ({:.5})",
                    buy_price, min_qty
                );
                continue;
            }

            let is_initial_buy = false; // To nie jest zlecenie startowe

            match place_binance_order(
                &client, &api_key, &secret_key, symbol, "BUY", buy_price, buy_quantity, is_initial_buy
            ).await {
                Ok(order_id) => println!("✅ Buy order placed: {} at {:.2} USDT", order_id, buy_price),
                Err(e) => println!("❌ Failed to place additional buy order: {}", e),
            }
        }
    }
}








use tokio::time::{sleep};
const TRADING_FEE_RATE: f64 = 0.015; // 1.5%

pub(crate) async fn monitor_and_reinvest(db: &mut Connection) {
    let api_key = load_config("config.txt").get("BINANCE_API_KEY").unwrap().clone();
    let secret_key = load_config("config.txt").get("BINANCE_SECRET_KEY").unwrap().clone();
    let client = Client::new();
    println!("🚀 [DEBUG] Rozpoczynamy monitorowanie zleceń i reinwestowanie...");
    loop {
        println!("🔄 Sprawdzanie sprzedanych pozycji...");

        let filled_orders = get_filled_sell_orders(db).await;

        for (symbol, sell_price, quantity) in filled_orders {
            let profit = sell_price * quantity * 1.015; // 📌 Uwzględniamy 1.5% zysku
            let reinvest_amount = (sell_price * quantity) + profit;

            println!(
                "✅ Sprzedano {} | Zysk: {:.2} USDT | Nowy kapitał: {:.2}",
                symbol, profit, reinvest_amount
            );

            let reinvest_price = sell_price * 0.95;
            let (min_qty, step_size) = get_lot_size(&symbol).await.unwrap_or((0.01, 0.01));
            let adjusted_quantity = adjust_quantity(reinvest_amount / reinvest_price, step_size);

            if adjusted_quantity < min_qty {
                println!(
                    "⚠️ Zbyt mała ilość do reinwestycji ({:.5}) dla {}",
                    adjusted_quantity, symbol
                );
                continue;
            }

            let is_initial_buy = false;

            let buy_order_id = place_binance_order(
                &client, &api_key, &secret_key, &symbol, "BUY", reinvest_price, adjusted_quantity, is_initial_buy
            ).await.unwrap_or(0);

            if buy_order_id > 0 {
                db.execute(
                    "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit, order_id)
                     VALUES (?1, ?2, ?3, datetime('now'), 'Buy', NULL, ?4)",
                    params![symbol, reinvest_price, adjusted_quantity, buy_order_id],
                ).expect("❌ Błąd zapisu do bazy");

                // 📌 Wystawiamy nowe zlecenie sprzedaży
                let sell_price = reinvest_price * 1.05;

                let sell_order_id = place_binance_order(
                    &client, &api_key, &secret_key, &symbol, "SELL", sell_price, adjusted_quantity, false
                ).await.unwrap_or(0);

                if sell_order_id > 0 {
                    db.execute(
                        "INSERT INTO trades (symbol, price, quantity, timestamp, type, profit, order_id)
                         VALUES (?1, ?2, ?3, datetime('now'), 'Sell', NULL, ?4)",
                        params![symbol, sell_price, adjusted_quantity, sell_order_id],
                    ).expect("❌ Błąd zapisu do bazy");

                    println!("📈 Wystawiono nowe zlecenie sprzedaży dla {} na {:.2}", symbol, sell_price);
                }
            }
        }

        sleep(Duration::from_secs(60)).await;
    }
}




