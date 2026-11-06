use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Error, anyhow};
use cdk::Amount;
use cdk::amount::SplitTarget;
use cdk::mint_url::MintUrl;
use cdk::nuts::nut00::KnownMethod;
use cdk::nuts::{
    CurrencyUnit, Id, KeySetInfo, MeltQuoteState, MintQuoteState, PaymentMethod, Proof, Proofs,
    SecretKey, SpendingConditions, State,
};
use cdk::secret::Secret;
use cdk::wallet::types::ProofInfo;
use cdk::wallet::{ReceiveOptions, SendOptions, Wallet};
use futures::StreamExt;
use nostr::nips::nip60::{CashuProof, TokenEvent, TransactionDirection};
use nostr_sdk::prelude::*;

use crate::keys;

/// How long to wait for relays when fetching a recipient's wallet info
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// A job mutating the wallet store or reconciling it with relays.
#[derive(Debug, Clone)]
pub(crate) enum Job {
    Reconcile,
    CreateQuote {
        mint: MintUrl,
        amount: u64,
    },
    ClaimQuote {
        mint: MintUrl,
        quote_id: String,
    },
    Melt {
        mint: MintUrl,
        invoice: String,
    },
    SendToken {
        mint: MintUrl,
        amount: u64,
    },
    SendNutzap {
        recipient: PublicKey,
        amount: u64,
        target: Option<EventId>,
        comment: Option<String>,
    },
    Receive {
        mint: MintUrl,
        token: String,
    },
    RedeemNutzap {
        mint: MintUrl,
        proofs: Proofs,
        nutzap_id: EventId,
        sender: PublicKey,
    },
}

impl Job {
    pub(crate) async fn run<S>(self, ctx: JobCtx<S>) -> Result<Outcome, Error>
    where
        S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + Send + Sync + 'static,
    {
        match self {
            Job::Reconcile => ctx.reconcile().await,
            Job::CreateQuote { mint, amount } => {
                let wallet = ctx.wallet(&mint)?;
                let quote = wallet
                    .mint_quote(
                        PaymentMethod::Known(KnownMethod::Bolt11),
                        Some(Amount::from(amount)),
                        None,
                        None,
                    )
                    .await?;

                Ok(Outcome::QuoteCreated {
                    mint,
                    quote_id: quote.id,
                    request: quote.request,
                    amount,
                })
            }
            Job::ClaimQuote { mint, quote_id } => {
                let wallet = ctx.wallet(&mint)?;
                let quote = wallet.check_mint_quote(&quote_id).await?;

                if quote.state != MintQuoteState::Paid {
                    return Ok(Outcome::Idle);
                }

                let proofs = wallet.mint(&quote_id, SplitTarget::default(), None).await?;
                let amount: u64 = proofs.iter().map(|proof| u64::from(proof.amount)).sum();

                let update = ctx
                    .sync_token_events(
                        wallet,
                        nostr_mint(&mint)?,
                        &ctx.previous_ids(&mint),
                        Some(keys::History {
                            direction: TransactionDirection::In,
                            amount: u64::from(quote.amount.unwrap_or_default()),
                            redeemed: Vec::new(),
                            sender: None,
                        }),
                    )
                    .await?;

                Ok(Outcome::QuoteMinted {
                    quote_id,
                    update,
                    amount,
                })
            }
            Job::Melt { mint, invoice } => {
                let wallet = ctx.wallet(&mint)?;
                let quote = wallet
                    .melt_quote(
                        PaymentMethod::Known(KnownMethod::Bolt11),
                        invoice,
                        None,
                        None,
                    )
                    .await?;

                let finalized = wallet
                    .prepare_melt(&quote.id, HashMap::new())
                    .await?
                    .confirm()
                    .await?;

                if finalized.state() != MeltQuoteState::Paid {
                    return Err(anyhow!("melt did not settle: {:?}", finalized.state()));
                }

                let amount = u64::from(finalized.total_amount());
                let update = ctx
                    .sync_token_events(
                        wallet,
                        nostr_mint(&mint)?,
                        &ctx.previous_ids(&mint),
                        Some(keys::History {
                            direction: TransactionDirection::Out,
                            amount,
                            redeemed: Vec::new(),
                            sender: None,
                        }),
                    )
                    .await?;

                Ok(Outcome::Melted { update, amount })
            }
            Job::SendToken { mint, amount } => {
                let wallet = ctx.wallet(&mint)?;
                let prepared = wallet
                    .prepare_send(Amount::from(amount), SendOptions::default())
                    .await?;
                let token = prepared.confirm(None).await?.to_string();

                let update = ctx
                    .sync_token_events(
                        wallet,
                        nostr_mint(&mint)?,
                        &ctx.previous_ids(&mint),
                        Some(keys::History {
                            direction: TransactionDirection::Out,
                            amount,
                            redeemed: Vec::new(),
                            sender: None,
                        }),
                    )
                    .await?;

                Ok(Outcome::Sent { update, token })
            }
            Job::SendNutzap {
                recipient,
                amount,
                target,
                comment,
            } => {
                let (mint_url, mint_tag, p2pk) = recipient_wallet_info(&ctx, recipient).await?;
                let wallet = ctx.wallet(&mint_url)?;

                let keysets = wallet
                    .keysets(cdk::wallet::types::KeysetLoadPolicy::CacheOnly)
                    .await?;

                let infos: Vec<KeySetInfo> = keysets
                    .iter()
                    .map(|keyset| KeySetInfo {
                        id: keyset.id,
                        unit: keyset.unit.clone(),
                        active: keyset.active.unwrap_or(true),
                        input_fee_ppk: keyset.input_fee_ppk,
                        final_expiry: keyset.final_expiry,
                    })
                    .collect();

                let options = SendOptions {
                    conditions: Some(SpendingConditions::new_p2pk(p2pk, None)),
                    ..Default::default()
                };

                let token = wallet
                    .prepare_send(Amount::from(amount), options)
                    .await?
                    .confirm(None)
                    .await?;
                let proofs = token.proofs(&infos)?;

                let mut tags = Vec::new();
                for proof in &proofs {
                    tags.push(serde_json::to_string(proof)?);
                }

                let event = keys::build_nutzap_event(
                    &ctx.signer,
                    recipient,
                    &mint_tag,
                    &tags,
                    target,
                    comment.as_deref().unwrap_or(""),
                )
                .await?;
                ctx.client.send_event(&event).await?;

                let update = ctx
                    .sync_token_events(
                        wallet,
                        nostr_mint(&mint_url)?,
                        &ctx.previous_ids(&mint_url),
                        Some(keys::History {
                            direction: TransactionDirection::Out,
                            amount,
                            redeemed: Vec::new(),
                            sender: None,
                        }),
                    )
                    .await?;

                Ok(Outcome::NutzapSent { update, amount })
            }
            Job::Receive { mint, token } => {
                let wallet = ctx.wallet(&mint)?;
                let received = wallet.receive(&token, ReceiveOptions::default()).await?;

                let update = ctx
                    .sync_token_events(
                        wallet,
                        nostr_mint(&mint)?,
                        &ctx.previous_ids(&mint),
                        Some(keys::History {
                            direction: TransactionDirection::In,
                            amount: u64::from(received),
                            redeemed: Vec::new(),
                            sender: None,
                        }),
                    )
                    .await?;

                Ok(Outcome::Received {
                    update,
                    amount: u64::from(received),
                })
            }
            Job::RedeemNutzap {
                mint,
                proofs,
                nutzap_id,
                sender,
            } => {
                let wallet = ctx.wallet(&mint)?;
                let options = ReceiveOptions {
                    p2pk_signing_keys: vec![ctx.wallet_key.clone()],
                    ..Default::default()
                };
                let received = wallet.receive_proofs(proofs, options, None, None).await?;

                let update = ctx
                    .sync_token_events(
                        wallet,
                        nostr_mint(&mint)?,
                        &ctx.previous_ids(&mint),
                        Some(keys::History {
                            direction: TransactionDirection::In,
                            amount: u64::from(received),
                            redeemed: vec![nutzap_id],
                            sender: Some(sender),
                        }),
                    )
                    .await?;

                Ok(Outcome::NutzapRedeemed {
                    update,
                    nutzap_id,
                    amount: u64::from(received),
                })
            }
        }
    }
}

pub(crate) struct TokenUpdate {
    pub(crate) event_id: EventId,
    pub(crate) token: TokenEvent,
    pub(crate) destroyed: Vec<EventId>,
}

pub(crate) enum Outcome {
    /// Nothing changed (e.g. an unpaid quote)
    Idle,
    /// Token events published by a reconcile pass
    Reconciled {
        updates: Vec<TokenUpdate>,
    },
    QuoteCreated {
        mint: MintUrl,
        quote_id: String,
        request: String,
        amount: u64,
    },
    QuoteMinted {
        quote_id: String,
        update: TokenUpdate,
        amount: u64,
    },
    Melted {
        update: TokenUpdate,
        amount: u64,
    },
    Sent {
        update: TokenUpdate,
        token: String,
    },
    NutzapSent {
        update: TokenUpdate,
        amount: u64,
    },
    Received {
        update: TokenUpdate,
        amount: u64,
    },
    NutzapRedeemed {
        update: TokenUpdate,
        nutzap_id: EventId,
        amount: u64,
    },
}

pub(crate) struct JobCtx<S> {
    pub(crate) wallets: HashMap<MintUrl, Wallet>,
    /// Wallet signing key, unlocking P2PK-locked nutzap proofs
    pub(crate) wallet_key: SecretKey,
    pub(crate) signer: S,
    pub(crate) client: Client,
    /// Relay token events at schedule time.
    pub(crate) tokens: HashMap<EventId, TokenEvent>,
}

impl<S> JobCtx<S>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + Sync,
{
    fn wallet(&self, mint: &MintUrl) -> Result<&Wallet, Error> {
        self.wallets
            .get(mint)
            .ok_or_else(|| anyhow!("no open wallet for mint {mint}"))
    }

    fn previous_ids(&self, mint: &MintUrl) -> Vec<EventId> {
        previous_ids(&self.tokens, mint)
    }

    /// Publish the store's current unspent set as a fresh token event.
    async fn sync_token_events(
        &self,
        wallet: &Wallet,
        mint: Url,
        previous: &[EventId],
        history: Option<keys::History>,
    ) -> Result<TokenUpdate, Error> {
        let me = self.signer.get_public_key_async().await?;
        let unspent = wallet.get_unspent_proofs().await?;

        let proofs = unspent
            .iter()
            .map(cashu_proof)
            .collect::<Result<Vec<_>, _>>()?;

        let mut token = TokenEvent::new(mint, proofs);
        token.del = previous.iter().map(|id| id.to_hex()).collect();
        let event = keys::build_token_event(&self.signer, &me, &token).await?;
        self.client.send_event(&event).await?;

        if !previous.is_empty() {
            let deletion = keys::build_token_deletion(&self.signer, previous).await?;
            self.client.send_event(&deletion).await?;
        }

        if let Some(history) = history {
            let history_event =
                keys::build_history_event(&self.signer, &me, &history, event.id, previous).await?;
            self.client.send_event(&history_event).await?;
        }

        Ok(TokenUpdate {
            event_id: event.id,
            token,
            destroyed: previous.to_vec(),
        })
    }

    /// Claim paid quotes, then converge each mint.
    async fn reconcile(self) -> Result<Outcome, Error> {
        let mut tokens = self.tokens.clone();
        let mut updates = Vec::new();

        for (mint_url, wallet) in &self.wallets {
            if let Err(error) = wallet.recover_incomplete_sagas().await {
                log::warn!("saga recovery failed for {mint_url}: {error:#}");
            }

            let minted = wallet.mint_unissued_quotes().await?;
            if minted == Amount::ZERO {
                continue;
            }

            let update = self
                .sync_token_events(
                    wallet,
                    nostr_mint(mint_url)?,
                    &previous_ids(&tokens, mint_url),
                    Some(keys::History {
                        direction: TransactionDirection::In,
                        amount: u64::from(minted),
                        redeemed: Vec::new(),
                        sender: None,
                    }),
                )
                .await?;

            for id in &update.destroyed {
                tokens.remove(id);
            }
            tokens.insert(update.event_id, update.token.clone());
            updates.push(update);
        }

        for (mint_url, wallet) in &self.wallets {
            let mut relay_ys = HashSet::new();
            let mut added = Vec::new();

            for token in tokens
                .values()
                .filter(|token| token.mint.as_str() == mint_url.to_string())
            {
                for proof in &token.proofs {
                    let info = match proof_info(proof, mint_url) {
                        Ok(info) => info,
                        Err(error) => {
                            log::warn!("skipping proof: {error:#}");
                            continue;
                        }
                    };
                    if relay_ys.insert(info.y) {
                        added.push(info);
                    }
                }
            }

            let unspent = wallet.get_unspent_proofs().await?;
            let removed = unspent
                .iter()
                .filter(|proof| match proof.y() {
                    Ok(y) => !relay_ys.contains(&y),
                    Err(_) => false,
                })
                .map(|proof| proof.y())
                .collect::<Result<Vec<_>, _>>()?;

            wallet.localstore.update_proofs(added, removed).await?;
        }

        Ok(Outcome::Reconciled { updates })
    }
}

fn previous_ids(tokens: &HashMap<EventId, TokenEvent>, mint: &MintUrl) -> Vec<EventId> {
    tokens
        .iter()
        .filter(|(_, token)| token.mint.as_str() == mint.to_string())
        .map(|(id, _)| *id)
        .collect()
}

/// Resolve a recipient's 10019 wallet info and pick a mint we share with them.
async fn recipient_wallet_info<S>(
    ctx: &JobCtx<S>,
    recipient: PublicKey,
) -> Result<(MintUrl, String, cdk::nuts::PublicKey), Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + Sync,
{
    let filter = Filter::new()
        .author(recipient)
        .kind(Kind::Custom(10019))
        .limit(1);

    let info = match ctx
        .client
        .database()
        .query(filter.clone())
        .await?
        .into_iter()
        .next()
    {
        Some(event) => event,
        None => {
            let mut stream = ctx
                .client
                .stream_events(filter)
                .timeout(FETCH_TIMEOUT)
                .await?;
            let mut found: Option<Event> = None;

            while let Some((_url, result)) = stream.next().await {
                if let Ok(event) = result {
                    let newer = found
                        .as_ref()
                        .is_none_or(|current| event.created_at > current.created_at);

                    if newer {
                        found = Some(event);
                    }
                }
            }

            found.ok_or_else(|| anyhow!("recipient has no wallet info"))?
        }
    };

    let (p2pk, mints) = parse_nutzap_info(&info)?;

    for mint in mints {
        let Ok(mint_url) = MintUrl::from_str(&mint) else {
            continue;
        };

        if ctx.wallets.contains_key(&mint_url) {
            return Ok((mint_url, mint, p2pk));
        }
    }

    Err(anyhow!("no shared mint with the recipient"))
}

fn parse_nutzap_info(event: &Event) -> Result<(cdk::nuts::PublicKey, Vec<String>), Error> {
    let mut p2pk = None;
    let mut mints = Vec::new();

    for tag in event.tags.iter() {
        let Some(value) = tag.content() else {
            continue;
        };

        match tag.kind() {
            "pubkey" if p2pk.is_none() => {
                p2pk = Some(cdk::nuts::PublicKey::from_str(value)?);
            }
            "mint" => mints.push(value.to_string()),
            _ => {}
        }
    }

    let Some(p2pk) = p2pk else {
        return Err(anyhow!("recipient has no wallet pubkey"));
    };

    Ok((p2pk, mints))
}

fn cashu_proof(proof: &Proof) -> Result<CashuProof, Error> {
    Ok(CashuProof {
        id: proof.keyset_id.to_string(),
        amount: u64::from(proof.amount),
        secret: proof.secret.to_string(),
        c: proof.c.to_string(),
    })
}

fn nostr_mint(mint: &MintUrl) -> Result<Url, Error> {
    Url::parse(&mint.to_string()).map_err(|error| anyhow!("invalid mint url: {error}"))
}

fn proof_info(proof: &CashuProof, mint: &MintUrl) -> Result<ProofInfo, Error> {
    let keyset_id = Id::from_str(&proof.id)?;
    let c = cdk::nuts::PublicKey::from_str(&proof.c)?;
    let info = ProofInfo::new(
        Proof::new(
            Amount::from(proof.amount),
            keyset_id,
            Secret::new(proof.secret.clone()),
            c,
        ),
        mint.clone(),
        State::Unspent,
        CurrencyUnit::Sat,
    )?;

    Ok(info)
}
