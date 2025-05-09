use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use futures::{io::BufReader, AsyncBufReadExt, AsyncReadExt, StreamExt, TryStreamExt};
use log::error;
use log::info;

use release_channel::AppVersion;
use reqwest_client::ReqwestClient;

use axum::{
    body::StreamBody,
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing, Router, Server,
};
use serde_json::Value;

mod llm_client;
use llm_client::LlmClient;

type AppState = Arc<LlmClient>;

// --- API Handlers ---
async fn index(State(_state): State<AppState>) -> (StatusCode, &'static str) {
    return (StatusCode::OK, "Ok");
}

async fn get_token(State(state): State<AppState>) -> (StatusCode, String) {
    let token = state.get_token().await;
    if let Ok(token) = token {
        info!("[/token] Successfully retrieved LLM token");
        return (StatusCode::OK, token);
    } else {
        error!("[/token] Failed to get LLM token");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to get LLM token".to_string());
    }
}

async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let mut response = match state.get("/models").await {
        Ok(response) => response,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let mut body = String::new();
    let text = match response.body_mut().read_to_string(&mut body).await {
        Ok(_) => {
            info!("[/v1/models] Successfully retrieved models list");
            body
        }
        Err(e) => {
            error!("[/v1/models] Failed to read response body: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to read response body".to_string()).into_response();
        }
    };

    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(data) => (StatusCode::OK, Json(data)).into_response(),
        Err(_) => (StatusCode::OK, text).into_response(),
    }
}

async fn chat_completion(
    State(state): State<AppState>,
    Json(payload): Json<Value>, // Deserialize JSON payload
) -> impl IntoResponse {
    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("claude-3-5-sonnet-latest");

    let payload = serde_json::to_string(&payload).unwrap();
    let payload = serde_json::value::RawValue::from_string(payload).unwrap();

    let body = serde_json::to_string(&client::PerformCompletionParams {
        provider: client::LanguageModelProvider::Anthropic,
        model: model.to_string(),
        provider_request: payload,
    })
    .unwrap();

    info!("[/v1/messages] Performing completion...");

    let response = match state.post("/completion", body).await {
        Ok(res) => res,
        Err(e) => {
            error!("[/v1/messages] Failed to perform completion: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to perform completion: {}", e)));
        }
    };

    if response.status().is_success() {
        let reader = BufReader::new(response.into_body());
        let lines_stream = reader.lines();

        // Convert each line to Server-Sent Events format with "data: " prefix
        let stream = lines_stream
            .map(|line_result| {
                line_result.map(|line| {
                    // Try to parse the line as JSON to check for the "type" field
                    let formatted_line = match serde_json::from_str::<serde_json::Value>(&line) {
                        Ok(json) => match json.get("type").and_then(|v| v.as_str()) {
                            Some(event_type) => format!("event: {}\ndata: {}\n\n", event_type, line),
                            _ => format!("data: {}\n\n", line),
                        },
                        Err(_) => format!("data: {}\n\n", line),
                    };

                    bytes::Bytes::from(formatted_line)
                })
            })
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));

        // Build the final Axum response
        let response_builder = Response::builder().status(StatusCode::OK);
        let response = match response_builder.body(StreamBody::new(stream)) {
            Ok(res) => res,
            Err(e) => {
                error!("[/v1/messages] Failed to build streaming response: {}", e);
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to construct streaming response".to_string()));
            }
        };

        info!("[/v1/messages] Successfully completion response to client");
        return Ok(response);
    } else {
        let status = response.status();
        error!("[/v1/messages] Completion API request failed with status: {:?}", status);
        return Err((
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            "Completion API request failed".to_string(),
        ));
    }
}
// --- End API Handlers ---

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about = "Zed LLM Server")]
struct Args {
    /// IP address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    ip: String,

    /// Port to listen on
    #[arg(long, short, default_value_t = 3000)]
    port: u16,
}

async fn run_web_server(llm_client: Arc<LlmClient>, args: Args) {
    let ip_addr = match IpAddr::from_str(&args.ip) {
        Ok(ip) => ip,
        Err(e) => {
            error!("Invalid IP address '{}': {}", args.ip, e);
            std::process::exit(1);
        }
    };
    let addr = SocketAddr::new(ip_addr, args.port);
    info!("Starting Axum Web server on {}", addr);

    let app_state: AppState = llm_client;
    let app = Router::new()
        .route("/", routing::get(index))
        .route("/token", routing::get(get_token))
        .route("/v1/models", routing::get(list_models))
        .route("/v1/messages", routing::post(chat_completion))
        .with_state(app_state);
    let server = Server::bind(&addr).serve(app.into_make_service());

    info!("Server running. Press Ctrl+C to stop.");
    server.await.unwrap();
}

async fn authenticate(client: Arc<client::Client>, cx: &gpui::AsyncAppContext) -> Result<()> {
    if client.has_credentials(&cx).await {
        client.authenticate_and_connect(true, &cx).await?;
    } else {
        client.authenticate_and_connect(false, &cx).await?;
    }

    let Some(user_id) = client.user_id() else {
        return Err(anyhow::anyhow!("User not authenticated"));
    };
    info!("Successfully authenticated user (ID: {})", user_id);
    Ok(())
}

fn run_zed_app(args: Args) {
    gpui::App::headless().run(move |cx| {
        info!("Zed Headless App starting...");
        let app_version = AppVersion::init(std::env!("CARGO_PKG_VERSION"));
        info!("App version: {}", app_version);
        release_channel::init(app_version, cx);

        settings::init(cx);
        client::init_settings(cx);

        let http_client = Arc::new(ReqwestClient::new());
        cx.set_http_client(http_client.clone());
        let client = client::Client::production(cx);

        // Share LlmClient with the web server
        let llm_client = Arc::new(LlmClient::new(client.clone()));

        cx.spawn(|cx| async move {
            if let Err(err) = authenticate(client, &cx).await {
                error!("Authentication failed: {}", err);
                error!("Please restart the server to try again.");
            } else {
                info!("Authentication successful. Starting web server...");
                run_web_server(llm_client, args).await;
            }
        })
        .detach();
    });
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    if let Err(e) = IpAddr::from_str(&args.ip) {
        error!("Invalid IP address '{}': {}", args.ip, e);
        std::process::exit(1);
    };
    run_zed_app(args);
}
