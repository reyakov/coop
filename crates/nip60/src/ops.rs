use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use anyhow::{Error, anyhow};
use cdk::Amount;
use cdk::amount::SplitTarget;
use cdk::mint_url::MintUrl;
use cdk::nuts::nut00::KnownMethod;
use cdk::nuts::{
    CurrencyUnit, Id, MeltQuoteState, MintQuoteState, PaymentMethod, Proof, Proofs, SecretKey,
    State,
};
use cdk::secret::Secret;
use cdk::wallet::types::ProofInfo;
use cdk::wallet::{ReceiveOptions, SendOptions, Wallet};
use nostr::nips::nip60::{CashuProof, TokenEvent, TransactionDirection};
use nostr_sdk::prelude::*;

use crate::keys;

/// A job mutating the wallet store or reconciling it with relays.
#[derive(Debug, Clone)]
pub(crate) enum Job {
    Reconcile,
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

/// History event payload for a token state change
pub(crate) struct History {
    pub(crate) direction: TransactionDirection,
    pub(crate) amount: u64,
    /// NIP-61 nutzap events redeemed by this state change
    pub(crate) redeemed: Vec<EventId>,
    /// Author of the redeemed nutzap, for the unencrypted `p` tag
    pub(crate) sender: Option<PublicKey>,
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

pub(crate) async fn run_job<S>(job: Job, ctx: JobCtx<S>) -> Result<Outcome, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + Send + Sync + 'static,
{
    match job {
        Job::Reconcile => reconcile(ctx).await,
        Job::ClaimQuote { mint, quote_id } => {
            let wallet = wallet_for(&ctx, &mint)?;
            let quote = wallet.check_mint_quote(&quote_id).await?;

            if quote.state != MintQuoteState::Paid {
                return Ok(Outcome::Idle);
            }

            let proofs = wallet.mint(&quote_id, SplitTarget::default(), None).await?;
            let amount: u64 = proofs.iter().map(|proof| u64::from(proof.amount)).sum();

            let update = sync_token_events(
                &ctx.signer,
                &ctx.client,
                wallet,
                nostr_mint(&mint)?,
                &previous_ids(&ctx.tokens, &mint),
                Some(History {
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
            let wallet = wallet_for(&ctx, &mint)?;
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
            let update = sync_token_events(
                &ctx.signer,
                &ctx.client,
                wallet,
                nostr_mint(&mint)?,
                &previous_ids(&ctx.tokens, &mint),
                Some(History {
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
            let wallet = wallet_for(&ctx, &mint)?;
            let prepared = wallet
                .prepare_send(Amount::from(amount), SendOptions::default())
                .await?;
            let token = prepared.confirm(None).await?.to_string();

            let update = sync_token_events(
                &ctx.signer,
                &ctx.client,
                wallet,
                nostr_mint(&mint)?,
                &previous_ids(&ctx.tokens, &mint),
                Some(History {
                    direction: TransactionDirection::Out,
                    amount,
                    redeemed: Vec::new(),
                    sender: None,
                }),
            )
            .await?;

            Ok(Outcome::Sent { update, token })
        }
        Job::Receive { mint, token } => {
            let wallet = wallet_for(&ctx, &mint)?;
            let received = wallet.receive(&token, ReceiveOptions::default()).await?;

            let update = sync_token_events(
                &ctx.signer,
                &ctx.client,
                wallet,
                nostr_mint(&mint)?,
                &previous_ids(&ctx.tokens, &mint),
                Some(History {
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
            let wallet = wallet_for(&ctx, &mint)?;
            let options = ReceiveOptions {
                p2pk_signing_keys: vec![ctx.wallet_key.clone()],
                ..Default::default()
            };
            let received = wallet.receive_proofs(proofs, options, None, None).await?;

            let update = sync_token_events(
                &ctx.signer,
                &ctx.client,
                wallet,
                nostr_mint(&mint)?,
                &previous_ids(&ctx.tokens, &mint),
                Some(History {
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

/// Claim paid quotes, then converge each mint.
async fn reconcile<S>(ctx: JobCtx<S>) -> Result<Outcome, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + Sync,
{
    let mut tokens = ctx.tokens.clone();
    let mut updates = Vec::new();

    for (mint_url, wallet) in &ctx.wallets {
        let minted = wallet.mint_unissued_quotes().await?;
        if minted == Amount::ZERO {
            continue;
        }

        let update = sync_token_events(
            &ctx.signer,
            &ctx.client,
            wallet,
            nostr_mint(mint_url)?,
            &previous_ids(&tokens, mint_url),
            Some(History {
                direction: TransactionDirection::In,
                amount: u64::from(minted),
                redeemed: Vec::new(),
                sender: None,
            }),
        )
        .await?;

        apply_update(&mut tokens, &update);
        updates.push(update);
    }

    for (mint_url, wallet) in &ctx.wallets {
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

/// Publish the store's current unspent set as a fresh token event.
async fn sync_token_events<S>(
    signer: &S,
    client: &Client,
    wallet: &Wallet,
    mint: Url,
    previous: &[EventId],
    history: Option<History>,
) -> Result<TokenUpdate, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + Sync + ?Sized,
{
    let me = signer.get_public_key_async().await?;
    let unspent = wallet.get_unspent_proofs().await?;

    let mut proofs = Vec::new();
    for proof in &unspent {
        proofs.push(cashu_proof(proof)?);
    }

    let token = TokenEvent::new(mint, proofs);
    let event = keys::build_token_event(signer, &me, &token).await?;
    client.send_event(&event).await?;

    if !previous.is_empty() {
        let deletion = keys::build_token_deletion(signer, previous).await?;
        client.send_event(&deletion).await?;
    }

    if let Some(history) = history {
        let history_event = keys::build_history_event(
            signer,
            &me,
            history.direction,
            history.amount,
            event.id,
            previous,
            &history.redeemed,
            history.sender,
        )
        .await?;
        client.send_event(&history_event).await?;
    }

    Ok(TokenUpdate {
        event_id: event.id,
        token,
        destroyed: previous.to_vec(),
    })
}

fn apply_update(tokens: &mut HashMap<EventId, TokenEvent>, update: &TokenUpdate) {
    for id in &update.destroyed {
        tokens.remove(id);
    }
    tokens.insert(update.event_id, update.token.clone());
}

fn previous_ids(tokens: &HashMap<EventId, TokenEvent>, mint: &MintUrl) -> Vec<EventId> {
    tokens
        .iter()
        .filter(|(_, token)| token.mint.as_str() == mint.to_string())
        .map(|(id, _)| *id)
        .collect()
}

fn wallet_for<'a, S>(ctx: &'a JobCtx<S>, mint: &MintUrl) -> Result<&'a Wallet, Error> {
    ctx.wallets
        .get(mint)
        .ok_or_else(|| anyhow!("no open wallet for mint {mint}"))
}

fn nostr_mint(mint: &MintUrl) -> Result<Url, Error> {
    Url::parse(&mint.to_string()).map_err(|error| anyhow!("invalid mint url: {error}"))
}

fn cashu_proof(proof: &Proof) -> Result<CashuProof, Error> {
    Ok(CashuProof {
        id: proof.keyset_id.to_string(),
        amount: u64::from(proof.amount),
        secret: proof.secret.to_string(),
        c: proof.c.to_string(),
    })
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
