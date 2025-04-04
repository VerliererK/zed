use anyhow::Result;
use client::{Client, EXPIRED_LLM_TOKEN_HEADER_NAME, MAX_LLM_MONTHLY_SPEND_REACHED_HEADER_NAME};
use http_client::{AsyncBody, HttpClient, Method, Response};
use log::error;
use smol::{
    io::AsyncReadExt,
    lock::{RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard},
};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct LlmApiToken(Arc<RwLock<Option<String>>>);

impl LlmApiToken {
    pub async fn acquire(&self, client: &Arc<Client>) -> Result<String> {
        let lock = self.0.upgradable_read().await;
        if let Some(token) = lock.as_ref() {
            Ok(token.to_string())
        } else {
            Self::fetch(RwLockUpgradableReadGuard::upgrade(lock).await, client).await
        }
    }

    pub async fn refresh(&self, client: &Arc<Client>) -> Result<String> {
        Self::fetch(self.0.write().await, client).await
    }

    async fn fetch<'a>(mut lock: RwLockWriteGuard<'a, Option<String>>, client: &Arc<Client>) -> Result<String> {
        let response = client.request(proto::GetLlmToken {}).await?;
        *lock = Some(response.token.clone());
        Ok(response.token.clone())
    }
}

#[derive(Clone)]
pub struct LlmClient {
    client: Arc<Client>,
    llm_api_token: LlmApiToken,
}

impl LlmClient {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            llm_api_token: LlmApiToken::default(),
        }
    }

    pub async fn get_token(&self) -> Result<String> {
        self.llm_api_token.acquire(&self.client).await
    }

    pub async fn perform_request(&self, method: Method, path: &str, body: String) -> Result<Response<AsyncBody>> {
        let http_client = &self.client.http_client();

        let mut token = self.llm_api_token.acquire(&self.client).await?;
        let mut did_retry = false;

        let response = loop {
            let request_builder = http_client::Request::builder();
            let request = request_builder
                .method(method.clone())
                .uri(http_client.build_zed_llm_url(path, &[])?.as_ref())
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {token}"))
                .body(body.clone().into())?;
            let mut response = http_client.send(request).await?;
            if response.status().is_success() {
                break response;
            } else if !did_retry && response.headers().get(EXPIRED_LLM_TOKEN_HEADER_NAME).is_some() {
                did_retry = true;
                token = self.llm_api_token.refresh(&self.client).await?;
                continue;
            }

            if response.headers().get(MAX_LLM_MONTHLY_SPEND_REACHED_HEADER_NAME).is_some() {
                error!("Max monthly spend reached");
            }

            let mut body = String::new();
            response.body_mut().read_to_string(&mut body).await?;
            error!("[perform_request] {method} {path} failed with status {}: {body}", response.status());

            break response;
        };
        Ok(response)
    }

    pub async fn get(&self, path: &str) -> Result<Response<AsyncBody>> {
        self.perform_request(Method::GET, path, String::new()).await
    }

    pub async fn post(&self, path: &str, body: String) -> Result<Response<AsyncBody>> {
        self.perform_request(Method::POST, path, body).await
    }
}
