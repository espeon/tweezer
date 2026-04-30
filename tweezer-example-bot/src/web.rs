use std::net::SocketAddr;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::Html,
    routing::get,
};
use serde::Serialize;

use crate::db::Database;

// ---------------------------------------------------------------------------
// HTML dashboard
// ---------------------------------------------------------------------------

const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>tweezer dashboard</title>
    <style>
        * { box-sizing: border-box; margin: 0; padding: 0; }
        body { font-family: system-ui, -apple-system, sans-serif; background: #0f0f0f; color: #e0e0e0; padding: 2rem; max-width: 900px; margin: 0 auto; }
        h1 { font-size: 1.5rem; margin-bottom: 1.5rem; color: #fff; }
        h2 { font-size: 1.1rem; margin: 1.5rem 0 0.75rem; color: #aaa; }
        .stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 1rem; margin-bottom: 1.5rem; }
        .stat { background: #1a1a1a; padding: 1rem; border-radius: 8px; }
        .stat-value { font-size: 1.5rem; font-weight: 600; color: #4ecdc4; }
        .stat-label { font-size: 0.8rem; color: #888; margin-top: 0.25rem; }
        table { width: 100%; border-collapse: collapse; background: #1a1a1a; border-radius: 8px; overflow: hidden; }
        th, td { padding: 0.6rem 0.75rem; text-align: left; font-size: 0.85rem; }
        th { background: #222; color: #aaa; font-weight: 500; }
        tr + tr td { border-top: 1px solid #2a2a2a; }
        td { color: #ccc; }
        .cmd-name { color: #4ecdc4; font-weight: 500; }
        .quote-text { font-style: italic; }
        .muted { color: #666; }
    </style>
</head>
<body>
    <h1>tweezer dashboard</h1>
    <div class="stats" id="stats"></div>
    <h2>dynamic commands</h2>
    <table id="commands"></table>
    <h2>quotes</h2>
    <table id="quotes"></table>
    <script>
        async function load() {
            const res = await fetch('/api/stats');
            const data = await res.json();

            document.getElementById('stats').innerHTML = `
                <div class="stat"><div class="stat-value">${data.commands}</div><div class="stat-label">commands</div></div>
                <div class="stat"><div class="stat-value">${data.quotes}</div><div class="stat-label">quotes</div></div>
                <div class="stat"><div class="stat-value">${data.reminders}</div><div class="stat-label">reminders</div></div>
            `;

            const cmdRes = await fetch('/api/commands');
            const cmds = await cmdRes.json();
            document.getElementById('commands').innerHTML = `
                <tr><th>name</th><th>template</th></tr>
                ${cmds.map(c => `<tr><td class="cmd-name">!${c.name}</td><td>${c.template}</td></tr>`).join('')}
            `;

            const quoteRes = await fetch('/api/quotes');
            const quotes = await quoteRes.json();
            document.getElementById('quotes').innerHTML = `
                <tr><th>#</th><th>text</th><th>author</th></tr>
                ${quotes.map(q => `<tr><td>${q.id}</td><td class="quote-text">"${q.text}"</td><td class="muted">${q.author}</td></tr>`).join('')}
            `;
        }
        load();
        setInterval(load, 30000);
    </script>
</body>
</html>"#;

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Stats {
    commands: usize,
    quotes: usize,
    reminders: usize,
}

#[derive(Serialize)]
struct CommandEntry {
    name: String,
    template: String,
}

#[derive(Serialize)]
struct QuoteEntry {
    id: usize,
    text: String,
    author: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn api_stats(State(db): State<Database>) -> Result<Json<Stats>, StatusCode> {
    let commands = db.cmd_list().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.len();
    let quotes = db.quote_count().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let reminders = db.reminder_count().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(Stats { commands, quotes, reminders }))
}

async fn api_commands(State(db): State<Database>) -> Result<Json<Vec<CommandEntry>>, StatusCode> {
    let cmds = db.cmd_list().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entries = cmds.into_iter().map(|(name, template)| CommandEntry { name, template }).collect();
    Ok(Json(entries))
}

async fn api_quotes(State(db): State<Database>) -> Result<Json<Vec<QuoteEntry>>, StatusCode> {
    let quotes = db.quote_all().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entries = quotes.into_iter().map(|(id, text, author, _ts)| QuoteEntry { id, text, author }).collect();
    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

pub async fn serve(db: Database) -> Result<(), std::io::Error> {
    let port: u16 = std::env::var("DASHBOARD_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);

    let app = Router::new()
        .route("/", get(dashboard))
        .route("/api/stats", get(api_stats))
        .route("/api/commands", get(api_commands))
        .route("/api/quotes", get(api_quotes))
        .with_state(db);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("dashboard running on http://{addr}");

    axum::serve(listener, app).await?;
    Ok(())
}
