use std::sync::Arc;

use anyhow::Result;
use log::error;
use log::info;

use release_channel::AppVersion;
use reqwest_client::ReqwestClient;

mod llm_client;
use llm_client::LlmClient;

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

fn run_zed_app() {
    gpui::App::headless().run(move |cx| {
        info!("Zed Headless App running...");
        let app_version = AppVersion::init(std::env!("CARGO_PKG_VERSION"));
        info!("App version: {}", app_version);
        release_channel::init(app_version, cx);

        settings::init(cx);
        client::init_settings(cx);

        let http_client = Arc::new(ReqwestClient::new());
        cx.set_http_client(http_client.clone());
        let client = client::Client::production(cx);

        cx.spawn(|cx| async move {
            if let Err(err) = authenticate(client, &cx).await {
                error!("Failed to authenticate: {}\nPlease restart the server to try again.", err);
            }
        })
        .detach();
    });
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    run_zed_app();
}
