use std::sync::Arc;

use anyhow::Error;
use gpui::http_client::{AsyncBody, HttpClient};
use nostr_sdk::prelude::*;
use smol::io::AsyncReadExt;

#[allow(async_fn_in_trait)]
pub trait NostrAddress {
    /// Get the NIP-05 profile
    async fn profile(&self, client: &Arc<dyn HttpClient>) -> Result<Nip05Profile, Error>;

    /// Verify the NIP-05 address
    async fn verify(
        &self,
        client: &Arc<dyn HttpClient>,
        public_key: &PublicKey,
    ) -> Result<bool, Error>;
}

impl NostrAddress for Nip05Address {
    async fn profile(&self, client: &Arc<dyn HttpClient>) -> Result<Nip05Profile, Error> {
        let mut body = Vec::new();
        let mut res = client
            .get(self.url().as_str(), AsyncBody::default(), false)
            .await?;

        // Read the response body into a vector
        res.body_mut().read_to_end(&mut body).await?;

        // Parse the JSON response
        let json: Value = serde_json::from_slice(&body)?;

        let profile = Nip05Profile::from_json(self, &json)?;

        Ok(profile)
    }

    async fn verify(
        &self,
        client: &Arc<dyn HttpClient>,
        public_key: &PublicKey,
    ) -> Result<bool, Error> {
        let mut body = Vec::new();
        let mut res = client
            .get(self.url().as_str(), AsyncBody::default(), false)
            .await?;

        // Read the response body into a vector
        res.body_mut().read_to_end(&mut body).await?;

        // Parse the JSON response
        let json: Value = serde_json::from_slice(&body)?;

        // Verify the NIP-05 address
        let verified = nip05::verify_from_json(public_key, self, &json);

        Ok(verified)
    }
}
