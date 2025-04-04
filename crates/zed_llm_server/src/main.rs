use std::sync::Arc;

use anyhow::Result;
use log::error;
use log::info;

use release_channel::AppVersion;
use reqwest_client::ReqwestClient;

async fn authenticate(client: Arc<client::Client>, cx: &gpui::AsyncAppContext) -> Result<()> {
    if *client::ZED_DEVELOPMENT_AUTH {
        client.authenticate_and_connect(true, cx).await?;
    } else if client::IMPERSONATE_LOGIN.is_some() {
        client.authenticate_and_connect(false, cx).await?;
    }
    Ok(())
}

fn run_zed_app() {
    gpui::App::headless().run(move |cx| {
        info!("Zed Headless App running...");
        let app_version = AppVersion::init(std::env!("CARGO_PKG_VERSION"));
        info!("app_version: {}", app_version);
        release_channel::init(app_version, cx);

        settings::init(cx);
        client::init_settings(cx);

        let http_client = Arc::new(ReqwestClient::new());
        cx.set_http_client(http_client.clone());
        let client = client::Client::production(cx);

        cx.spawn(|cx| async move {
            match authenticate(client, &cx).await {
                Ok(_) => info!("Authenticated successfully"),
                Err(err) => error!("Failed to authenticate: {}", err),
            }
        })
        .detach();
    });
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    run_zed_app();
}
