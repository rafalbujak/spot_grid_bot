use std::time::Duration;
use dotenvy::dotenv;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use rusqlite::{params, Connection};
use crate::binance_api::{ get_filled_sell_orders, get_lot_size, get_price, place_binance_order};
use crate::utils::{get_user_input, load_config, adjust_quantity};
pub(crate) async fn execute_grid_trade(db: &mut Connection) {
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


pub(crate) async fn monitor_and_reinvest(db: &mut Connection) {
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


