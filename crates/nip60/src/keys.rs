use anyhow::{Error, anyhow};
use hkdf::Hkdf;
use nostr::nips::nip60::TokenEvent;
use nostr_sdk::prelude::*;
use sha2::Sha256;

const SEED_SALT: &[u8] = b"coop nip60 seed salt";
const SEED_INFO: &[u8] = b"coop cashu seed";

pub(crate) struct WalletData {
    pub(crate) event_id: EventId,
    pub(crate) created_at: Timestamp,
    pub(crate) privkey: SecretKey,
    pub(crate) mints: Vec<Url>,
}

impl Clone for WalletData {
    fn clone(&self) -> Self {
        Self {
            event_id: self.event_id,
            created_at: self.created_at,
            privkey: self.privkey.clone(),
            mints: self.mints.clone(),
        }
    }
}

pub(crate) fn generate_privkey() -> SecretKey {
    SecretKey::generate()
}

/// Derive the cdk seed from the wallet key.
pub(crate) fn derive_seed(privkey: &SecretKey) -> Result<[u8; 64], Error> {
    let ikm = privkey.to_secret_bytes();
    let hk = Hkdf::<Sha256>::new(Some(SEED_SALT), &ikm);
    let mut seed = [0u8; 64];
    hk.expand(SEED_INFO, &mut seed)
        .map_err(|error| anyhow!("seed derivation failed: {error}"))?;
    Ok(seed)
}

pub(crate) fn parse_wallet_content(content: &str) -> Result<(SecretKey, Vec<Url>), Error> {
    let rows: Vec<Vec<String>> =
        serde_json::from_str(content).map_err(|error| anyhow!("invalid wallet event: {error}"))?;

    let mut privkey = None;
    let mut mints = Vec::new();
    for row in rows {
        let (Some(key), Some(value)) = (row.first(), row.get(1)) else {
            continue;
        };
        match key.as_str() {
            "privkey" if privkey.is_none() => privkey = Some(SecretKey::from_hex(value)?),
            "mint" => {
                if let Ok(mint) = Url::parse(value) {
                    mints.push(mint);
                }
            }
            _ => {}
        }
    }

    let Some(privkey) = privkey else {
        return Err(anyhow!("wallet event is missing the private key"));
    };
    if mints.is_empty() {
        return Err(anyhow!("wallet event is missing mints"));
    }

    Ok((privkey, mints))
}

pub(crate) async fn decrypt_wallet_event<S>(
    signer: &S,
    me: &PublicKey,
    event: &Event,
) -> Result<WalletData, Error>
where
    S: AsyncNip44 + ?Sized,
{
    if event.pubkey != *me {
        return Err(anyhow!("wallet event was not published by this account"));
    }

    let content = signer.nip44_decrypt_async(me, &event.content).await?;
    let (privkey, mints) = parse_wallet_content(&content)?;

    Ok(WalletData {
        event_id: event.id,
        created_at: event.created_at,
        privkey,
        mints,
    })
}

pub(crate) async fn decrypt_token_event<S>(
    signer: &S,
    me: &PublicKey,
    event: &Event,
) -> Result<TokenEvent, Error>
where
    S: AsyncNip44 + ?Sized,
{
    if event.pubkey != *me {
        return Err(anyhow!("token event was not published by this account"));
    }

    let content = signer.nip44_decrypt_async(me, &event.content).await?;

    let token =
        serde_json::from_str(&content).map_err(|error| anyhow!("invalid token event: {error}"))?;

    Ok(token)
}

pub(crate) async fn build_wallet_event<S>(
    signer: &S,
    me: &PublicKey,
    privkey: &SecretKey,
    mints: &[Url],
) -> Result<Event, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let mut rows = vec![vec!["privkey".to_string(), privkey.to_secret_hex()]];
    for mint in mints {
        rows.push(vec!["mint".to_string(), mint.to_string()]);
    }

    let content = signer
        .nip44_encrypt_async(me, &serde_json::to_string(&rows)?)
        .await?;

    Ok(EventBuilder::new(Kind::CashuWallet, content)
        .finalize_async(signer)
        .await?)
}

pub(crate) async fn build_token_event<S>(
    signer: &S,
    me: &PublicKey,
    token: &TokenEvent,
) -> Result<Event, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let content = signer
        .nip44_encrypt_async(me, &serde_json::to_string(token)?)
        .await?;

    Ok(EventBuilder::new(Kind::CashuWalletUnspentProof, content)
        .finalize_async(signer)
        .await?)
}

/// NIP-09 deletion carrying the `k: 7375` tag required by NIP-60.
pub(crate) async fn build_token_deletion<S>(signer: &S, ids: &[EventId]) -> Result<Event, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let mut tags: Vec<Tag> = ids.iter().map(|id| Tag::event(*id)).collect();
    tags.push(Tag::custom("k", ["7375".to_string()]));

    Ok(EventBuilder::new(Kind::EventDeletion, "")
        .tags(tags)
        .finalize_async(signer)
        .await?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_history_event<S>(
    signer: &S,
    me: &PublicKey,
    direction: TransactionDirection,
    amount: u64,
    created: EventId,
    destroyed: &[EventId],
    redeemed: &[EventId],
    sender: Option<PublicKey>,
) -> Result<Event, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let mut rows = vec![
        vec!["direction".to_string(), direction.to_string()],
        vec!["amount".to_string(), amount.to_string()],
    ];

    for id in destroyed {
        rows.push(vec![
            "e".to_string(),
            id.to_hex(),
            String::new(),
            "destroyed".to_string(),
        ]);
    }

    rows.push(vec![
        "e".to_string(),
        created.to_hex(),
        String::new(),
        "created".to_string(),
    ]);

    let mut tags: Vec<Tag> = redeemed
        .iter()
        .map(|id| Tag::custom("e", [id.to_hex(), String::new(), "redeemed".to_string()]))
        .collect();

    if let Some(sender) = sender {
        tags.push(Tag::custom("p", [sender.to_hex()]));
    }

    let content = signer
        .nip44_encrypt_async(me, &serde_json::to_string(&rows)?)
        .await?;

    Ok(EventBuilder::new(Kind::CashuWalletSpendingHistory, content)
        .tags(tags)
        .finalize_async(signer)
        .await?)
}

pub(crate) async fn build_nutzap_info_event<S>(
    signer: &S,
    mints: &[Url],
    relays: &[&str],
    wallet_pubkey: PublicKey,
) -> Result<Event, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + ?Sized,
{
    let mut tags: Vec<Tag> = relays
        .iter()
        .map(|relay| Tag::custom("relay", [relay.to_string()]))
        .collect();

    for mint in mints {
        tags.push(Tag::custom("mint", [mint.to_string(), "sat".to_string()]));
    }

    tags.push(Tag::custom(
        "pubkey",
        [format!("02{}", wallet_pubkey.to_hex())],
    ));

    Ok(EventBuilder::new(Kind::Custom(10019), "")
        .tags(tags)
        .finalize_async(signer)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wallet_content() {
        let privkey = SecretKey::generate();
        let mint = Url::parse("https://mint.example.com").expect("valid mint URL");

        let content = serde_json::to_string(&vec![
            vec!["privkey".to_string(), privkey.to_secret_hex()],
            vec!["mint".to_string(), mint.to_string()],
        ])
        .expect("serialization cannot fail");

        let (parsed_key, mints) = parse_wallet_content(&content).expect("valid content");
        assert_eq!(parsed_key.to_secret_hex(), privkey.to_secret_hex());
        assert_eq!(mints, vec![mint]);
    }

    #[test]
    fn wallet_content_rejects_missing_privkey() {
        let mint = Url::parse("https://mint.example.com").expect("valid mint URL");
        let content = serde_json::to_string(&vec![vec!["mint".to_string(), mint.to_string()]])
            .expect("serialization cannot fail");

        assert!(parse_wallet_content(&content).is_err());
    }

    #[test]
    fn seed_is_deterministic_per_key() {
        let key = SecretKey::generate();
        let first = derive_seed(&key).expect("seed derivation cannot fail");
        let second = derive_seed(&key).expect("seed derivation cannot fail");
        assert_eq!(first, second);

        let other = derive_seed(&SecretKey::generate()).expect("seed derivation cannot fail");
        assert_ne!(first, other);
    }
}
