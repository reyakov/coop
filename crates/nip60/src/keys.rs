use anyhow::{Error, anyhow};
use hkdf::Hkdf;
use nostr::nips::nip60::TokenEvent;
use nostr_sdk::prelude::*;
use sha2::Sha256;

const SEED_SALT: &[u8] = b"coop nip60 seed salt";
const SEED_INFO: &[u8] = b"coop cashu seed";

/// Parsed 17375 wallet event: the wallet key and its mint list.
#[derive(Clone)]
pub(crate) struct WalletData {
    pub(crate) event_id: EventId,
    pub(crate) created_at: Timestamp,
    pub(crate) privkey: SecretKey,
    pub(crate) mints: Vec<Url>,
}

impl WalletData {
    /// A new wallet with a fresh key.
    pub(crate) fn generate(first_mint: Url) -> Self {
        Self {
            event_id: EventId::from_byte_array([0; 32]),
            created_at: Timestamp::from_secs(0),
            privkey: SecretKey::generate(),
            mints: vec![first_mint],
        }
    }

    /// The cdk seed, derived deterministically from the wallet key.
    pub(crate) fn derive_seed(privkey: &SecretKey) -> Result<[u8; 64], Error> {
        let ikm = privkey.to_secret_bytes();
        let hk = Hkdf::<Sha256>::new(Some(SEED_SALT), &ikm);
        let mut seed = [0u8; 64];
        hk.expand(SEED_INFO, &mut seed)
            .map_err(|error| anyhow!("seed derivation failed: {error}"))?;
        Ok(seed)
    }

    /// Decrypt and parse a 17375 event published by this account.
    pub(crate) async fn decrypt<S>(signer: &S, me: &PublicKey, event: &Event) -> Result<Self, Error>
    where
        S: AsyncNip44 + ?Sized,
    {
        if event.pubkey != *me {
            return Err(anyhow!("wallet event was not published by this account"));
        }

        let content = signer.nip44_decrypt_async(me, &event.content).await?;
        let (privkey, mints) = parse_wallet_content(&content)?;

        Ok(Self {
            event_id: event.id,
            created_at: event.created_at,
            privkey,
            mints,
        })
    }

    /// Build the encrypted 17375 event for this wallet data.
    pub(crate) async fn to_event<S>(&self, signer: &S, me: &PublicKey) -> Result<Event, Error>
    where
        S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
    {
        let mut rows = vec![vec!["privkey".to_string(), self.privkey.to_secret_hex()]];
        for mint in &self.mints {
            rows.push(vec!["mint".to_string(), mint.to_string()]);
        }

        let content = signer
            .nip44_encrypt_async(me, &serde_json::to_string(&rows)?)
            .await?;

        Ok(EventBuilder::new(Kind::CashuWallet, content)
            .finalize_async(signer)
            .await?)
    }
}

fn parse_wallet_content(content: &str) -> Result<(SecretKey, Vec<Url>), Error> {
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

/// Decrypted 7376 payload, enough for the wallet panel's history list.
pub(crate) struct HistoryPayload {
    pub(crate) direction: TransactionDirection,
    pub(crate) amount: u64,
}

pub(crate) async fn decrypt_history_event<S>(
    signer: &S,
    me: &PublicKey,
    event: &Event,
) -> Result<HistoryPayload, Error>
where
    S: AsyncNip44 + ?Sized,
{
    if event.pubkey != *me {
        return Err(anyhow!("history event was not published by this account"));
    }

    let content = signer.nip44_decrypt_async(me, &event.content).await?;
    let rows: Vec<Vec<String>> = serde_json::from_str(&content)
        .map_err(|error| anyhow!("invalid history event: {error}"))?;

    let mut direction = None;
    let mut amount = None;
    for row in rows {
        let (Some(key), Some(value)) = (row.first(), row.get(1)) else {
            continue;
        };
        match key.as_str() {
            "direction" if direction.is_none() => direction = value.parse().ok(),
            "amount" if amount.is_none() => amount = value.parse().ok(),
            _ => {}
        }
    }

    let Some(direction) = direction else {
        return Err(anyhow!("history event is missing the direction"));
    };
    let Some(amount) = amount else {
        return Err(anyhow!("history event is missing the amount"));
    };

    Ok(HistoryPayload { direction, amount })
}

/// Build the public kind 9321 nutzap event carrying P2PK-locked proofs.
pub(crate) async fn build_nutzap_event<S>(
    signer: &S,
    recipient: PublicKey,
    mint: &str,
    proofs: &[String],
    target: Option<EventId>,
    comment: &str,
) -> Result<Event, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + ?Sized,
{
    let mut tags: Vec<Tag> = vec![
        Tag::custom("p", [recipient.to_hex()]),
        Tag::custom("u", [mint.to_string()]),
        Tag::custom("unit", ["sat".to_string()]),
    ];

    for proof in proofs {
        tags.push(Tag::custom("proof", [proof.clone()]));
    }

    if let Some(target) = target {
        tags.push(Tag::custom("e", [target.to_hex()]));
    }

    Ok(EventBuilder::new(Kind::Custom(9321), comment)
        .tags(tags)
        .finalize_async(signer)
        .await?)
}

/// Encrypted 7376 history payload for one state change.
pub(crate) struct History {
    pub(crate) direction: TransactionDirection,
    pub(crate) amount: u64,
    /// NIP-61 nutzap events redeemed by this state change
    pub(crate) redeemed: Vec<EventId>,
    /// Author of the redeemed nutzap, for the unencrypted `p` tag
    pub(crate) sender: Option<PublicKey>,
}

pub(crate) async fn build_history_event<S>(
    signer: &S,
    me: &PublicKey,
    history: &History,
    created: EventId,
    destroyed: &[EventId],
) -> Result<Event, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let mut rows = vec![
        vec!["direction".to_string(), history.direction.to_string()],
        vec!["amount".to_string(), history.amount.to_string()],
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

    let mut tags: Vec<Tag> = history
        .redeemed
        .iter()
        .map(|id| Tag::custom("e", [id.to_hex(), String::new(), "redeemed".to_string()]))
        .collect();

    if let Some(sender) = history.sender {
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
    relays: &[RelayUrl],
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
        let first = WalletData::derive_seed(&key).expect("seed derivation cannot fail");
        let second = WalletData::derive_seed(&key).expect("seed derivation cannot fail");
        assert_eq!(first, second);

        let other =
            WalletData::derive_seed(&SecretKey::generate()).expect("seed derivation cannot fail");
        assert_ne!(first, other);
    }
}
