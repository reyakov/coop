use std::borrow::Cow;
use std::result::Result;
use std::sync::Arc;

use nostr_sdk::prelude::*;
use smol::lock::RwLock;

#[derive(Debug)]
pub struct CoopSigner {
    /// User's signer
    signer: RwLock<Arc<dyn NostrSigner>>,

    /// User's signer public key
    signer_pkey: RwLock<Option<PublicKey>>,

    /// Specific signer for encryption purposes
    encryption_signer: RwLock<Option<Arc<dyn NostrSigner>>>,
}

impl CoopSigner {
    pub fn new<T>(signer: T) -> Self
    where
        T: IntoNostrSigner,
    {
        Self {
            signer: RwLock::new(signer.into_nostr_signer()),
            signer_pkey: RwLock::new(None),
            encryption_signer: RwLock::new(None),
        }
    }

    /// Get the current signer.
    pub async fn get(&self) -> Arc<dyn NostrSigner> {
        self.signer.read().await.clone()
    }

    /// Get the encryption signer.
    pub async fn get_encryption_signer(&self) -> Option<Arc<dyn NostrSigner>> {
        self.encryption_signer.read().await.clone()
    }

    /// Get public key
    ///
    /// Ensure to call this method after the signer has been initialized.
    /// Otherwise, this method will panic.
    pub fn public_key(&self) -> Option<PublicKey> {
        *self.signer_pkey.read_blocking()
    }

    /// Switch the current signer to a new signer.
    pub async fn switch<T>(&self, new: T)
    where
        T: IntoNostrSigner,
    {
        let new_signer = new.into_nostr_signer();
        let public_key = new_signer.get_public_key().await.ok();
        let mut signer = self.signer.write().await;
        let mut signer_pkey = self.signer_pkey.write().await;
        let mut encryption_signer = self.encryption_signer.write().await;

        // Switch to the new signer
        *signer = new_signer;

        // Update the public key
        *signer_pkey = public_key;

        // Reset the encryption signer
        *encryption_signer = None;
    }

    /// Set the encryption signer.
    pub async fn set_encryption_signer<T>(&self, new: T)
    where
        T: IntoNostrSigner,
    {
        let mut encryption_signer = self.encryption_signer.write().await;
        *encryption_signer = Some(new.into_nostr_signer());
    }
}

impl NostrSigner for CoopSigner {
    #[allow(mismatched_lifetime_syntaxes)]
    fn backend(&self) -> SignerBackend {
        SignerBackend::Custom(Cow::Borrowed("custom"))
    }

    fn get_public_key<'a>(&'a self) -> BoxedFuture<'a, Result<PublicKey, SignerError>> {
        Box::pin(async move { self.get().await.get_public_key().await })
    }

    fn sign_event<'a>(
        &'a self,
        unsigned: UnsignedEvent,
    ) -> BoxedFuture<'a, Result<Event, SignerError>> {
        Box::pin(async move { self.get().await.sign_event(unsigned).await })
    }

    fn nip04_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move { self.get().await.nip04_encrypt(public_key, content).await })
    }

    fn nip04_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        encrypted_content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            self.get()
                .await
                .nip04_decrypt(public_key, encrypted_content)
                .await
        })
    }

    fn nip44_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move { self.get().await.nip44_encrypt(public_key, content).await })
    }

    fn nip44_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        payload: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move { self.get().await.nip44_decrypt(public_key, payload).await })
    }
}
