use std::fmt;

use data_encoding::BASE64;
use nostr::nips::nip44::v2::{ConversationKey, decrypt_to_bytes, encrypt_to_bytes_with_nonce};
use nostr_sdk::prelude::{
    AsyncGetPublicKey, AsyncNip44, AsyncSignEvent, Event, EventBuilder, EventId, FinalizeEvent,
    FinalizeEventAsync, Keys, Kind, PublicKey, Tag, Timestamp, UnsignedEvent,
};

use crate::derive::GroupKey;
use crate::{ChannelId, Epoch};

pub const KIND_WRAP: u16 = 1059;
pub const KIND_WRAP_EPHEMERAL: u16 = 21059;
pub const KIND_SEAL_ENCRYPTED: u16 = 20013;
pub const KIND_SEAL_PLAINTEXT: u16 = 20014;
pub const NIP44_MAX_PLAINTEXT: usize = 65_535;

const TAG_MS: &str = "ms";
const TAG_CHANNEL: &str = "channel";
const TAG_EPOCH: &str = "epoch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealForm {
    Encrypted,
    Plaintext,
}

impl SealForm {
    pub fn kind(self) -> u16 {
        match self {
            SealForm::Encrypted => KIND_SEAL_ENCRYPTED,
            SealForm::Plaintext => KIND_SEAL_PLAINTEXT,
        }
    }

    fn from_kind(kind: u16) -> Option<Self> {
        match kind {
            KIND_SEAL_ENCRYPTED => Some(SealForm::Encrypted),
            KIND_SEAL_PLAINTEXT => Some(SealForm::Plaintext),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum StreamError {
    Sign(String),
    Encrypt(String),
    Decrypt(String),
    Parse(String),
    Oversize(usize),
    BadWrapKind(u16),
    WrongStream,
    BadWrapSignature,
    BadSealKind(u16),
    BadSealSignature,
    AuthorMismatch,
    BadRumorId,
    BadMs,
    ChannelMismatch,
    EpochMismatch,
    MissingTag(&'static str),
    DuplicateTag(&'static str),
    NotRewrappable,
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::Sign(error) => write!(f, "sign: {error}"),
            StreamError::Encrypt(error) => write!(f, "encrypt: {error}"),
            StreamError::Decrypt(error) => write!(f, "decrypt: {error}"),
            StreamError::Parse(error) => write!(f, "parse: {error}"),
            StreamError::Oversize(len) => write!(f, "plaintext {len} bytes exceeds NIP-44 cap"),
            StreamError::BadWrapKind(kind) => write!(f, "not a wrap kind: {kind}"),
            StreamError::WrongStream => write!(f, "wrap author is not this stream"),
            StreamError::BadWrapSignature => write!(f, "restricted wrap signature invalid"),
            StreamError::BadSealKind(kind) => write!(f, "not a seal kind: {kind}"),
            StreamError::BadSealSignature => write!(f, "seal signature invalid"),
            StreamError::AuthorMismatch => write!(f, "rumor pubkey != seal pubkey"),
            StreamError::BadRumorId => write!(f, "rumor id != computed hash"),
            StreamError::BadMs => write!(f, "ms is not a canonical decimal in 0..=999"),
            StreamError::ChannelMismatch => write!(f, "channel binding mismatch"),
            StreamError::EpochMismatch => write!(f, "epoch binding mismatch"),
            StreamError::MissingTag(name) => write!(f, "missing rumor tag: {name}"),
            StreamError::DuplicateTag(name) => write!(f, "duplicate rumor tag: {name}"),
            StreamError::NotRewrappable => write!(f, "only plaintext seals survive re-wrapping"),
        }
    }
}

impl std::error::Error for StreamError {}

#[derive(Debug, Clone)]
pub struct OpenedStream {
    pub rumor_id: EventId,
    pub author: PublicKey,
    pub seal_form: SealForm,
    pub seal: Event,
    pub wrapper_id: EventId,
    pub at_ms: u64,
    pub rumor: UnsignedEvent,
}

pub fn split_ms(at_ms: u64) -> (u64, u16) {
    (at_ms / 1000, (at_ms % 1000) as u16)
}

/// Build a rumor carrying a full epoch-ms time: seconds in `created_at`, the remainder as `["ms", 0..=999]`.
pub fn build_rumor_ms(
    kind: u16,
    author: PublicKey,
    content: &str,
    mut tags: Vec<Tag>,
    at_ms: u64,
) -> UnsignedEvent {
    let (seconds, offset) = split_ms(at_ms);
    tags.push(Tag::custom(TAG_MS, [offset.to_string()]));
    build_rumor_secs(kind, author, content, tags, seconds)
}

/// Build a rumor with a plain seconds timestamp and no `ms` tag.
pub fn build_rumor_secs(
    kind: u16,
    author: PublicKey,
    content: &str,
    tags: Vec<Tag>,
    at_secs: u64,
) -> UnsignedEvent {
    let mut rumor = UnsignedEvent::new(
        author,
        Timestamp::from_secs(at_secs),
        Kind::Custom(kind),
        tags,
        content,
    );
    rumor.ensure_id();
    rumor
}

pub fn resolve_ms_strict(rumor: &UnsignedEvent) -> Result<u64, StreamError> {
    let seconds = rumor.created_at.as_secs().saturating_mul(1000);
    let mut tag: Option<Option<String>> = None;

    for candidate in rumor.tags.iter() {
        let fields = candidate.as_slice();
        if fields.first().map(String::as_str) == Some(TAG_MS) {
            tag = Some(fields.get(1).cloned());
            break;
        }
    }

    let Some(raw) = tag else {
        return Ok(seconds);
    };
    let raw = raw.ok_or(StreamError::BadMs)?;

    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(StreamError::BadMs);
    }

    let offset: u64 = raw.parse().map_err(|_| StreamError::BadMs)?;

    if offset > 999 || (raw.len() > 1 && raw.starts_with('0')) {
        return Err(StreamError::BadMs);
    }

    Ok(seconds.saturating_add(offset))
}

pub fn seal_content(
    rumor: &UnsignedEvent,
    form: SealForm,
    group: &GroupKey,
) -> Result<String, StreamError> {
    let json = rumor.as_json();
    check_plaintext_cap(json.len())?;

    match form {
        SealForm::Plaintext => Ok(json),
        SealForm::Encrypted => seal_bytes(group.conversation(), json.as_bytes()),
    }
}

pub fn seal_bytes(conversation: &ConversationKey, plaintext: &[u8]) -> Result<String, StreamError> {
    check_plaintext_cap(plaintext.len())?;
    Ok(BASE64.encode(&encrypt(conversation, plaintext)?))
}

pub fn open_bytes(conversation: &ConversationKey, content: &str) -> Result<Vec<u8>, StreamError> {
    let payload = BASE64
        .decode(content.as_bytes())
        .map_err(|error| StreamError::Decrypt(error.to_string()))?;

    decrypt_to_bytes(conversation, &payload)
        .map_err(|error| StreamError::Decrypt(error.to_string()))
}

/// A member's own document (the Community List, the Invite List): NIP-44 to self.
pub async fn seal_to_self<S>(signer: &S, plaintext: &str) -> Result<String, StreamError>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    check_plaintext_cap(plaintext.len())?;

    let address = signer
        .get_public_key_async()
        .await
        .map_err(|error| StreamError::Encrypt(error.to_string()))?;

    signer
        .nip44_encrypt_async(&address, plaintext)
        .await
        .map_err(|error| StreamError::Encrypt(error.to_string()))
}

pub async fn open_to_self<S>(signer: &S, content: &str) -> Result<String, StreamError>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    let address = signer
        .get_public_key_async()
        .await
        .map_err(|error| StreamError::Decrypt(error.to_string()))?;

    signer
        .nip44_decrypt_async(&address, content)
        .await
        .map_err(|error| StreamError::Decrypt(error.to_string()))
}

pub async fn build_seal<S>(
    rumor: &UnsignedEvent,
    form: SealForm,
    group: &GroupKey,
    author: &S,
) -> Result<Event, StreamError>
where
    S: AsyncGetPublicKey + AsyncSignEvent + ?Sized,
{
    let content = seal_content(rumor, form, group)?;
    EventBuilder::new(Kind::Custom(form.kind()), content)
        .custom_created_at(rumor.created_at)
        .finalize_async(author)
        .await
        .map_err(|error| StreamError::Sign(error.to_string()))
}

pub fn wrap_seal(
    seal: &Event,
    group: &GroupKey,
    wrap_kind: u16,
    at: Timestamp,
    extra: &[Tag],
) -> Result<(Event, Keys), StreamError> {
    wrap_seal_with(
        seal,
        group.conversation(),
        group.keys(),
        wrap_kind,
        at,
        extra,
    )
}

pub fn wrap_seal_with(
    seal: &Event,
    conversation: &ConversationKey,
    signer: &Keys,
    wrap_kind: u16,
    at: Timestamp,
    extra: &[Tag],
) -> Result<(Event, Keys), StreamError> {
    if wrap_kind != KIND_WRAP && wrap_kind != KIND_WRAP_EPHEMERAL {
        return Err(StreamError::BadWrapKind(wrap_kind));
    }

    let json = seal.as_json();
    check_plaintext_cap(json.len())?;

    let content = BASE64.encode(&encrypt(conversation, json.as_bytes())?);
    let ephemeral = Keys::generate();

    let mut tags = vec![Tag::public_key(ephemeral.public_key())];
    tags.extend_from_slice(extra);

    let wrap = EventBuilder::new(Kind::Custom(wrap_kind), content)
        .tags(tags)
        .custom_created_at(at)
        .finalize(signer)
        .map_err(|error| StreamError::Sign(error.to_string()))?;

    Ok((wrap, ephemeral))
}

pub fn rewrap_seal(
    seal: &Event,
    read: &GroupKey,
    signer: &GroupKey,
    at: Timestamp,
) -> Result<(Event, Keys), StreamError> {
    if seal.kind.as_u16() != KIND_SEAL_PLAINTEXT {
        return Err(StreamError::NotRewrappable);
    }

    wrap_seal_with(seal, read.conversation(), signer.keys(), KIND_WRAP, at, &[])
}

pub fn open_wrap(wrap: &Event, group: &GroupKey) -> Result<OpenedStream, StreamError> {
    open_wrap_at(wrap, &group.pk(), group.conversation(), false)
}

pub fn open_wrap_at(
    wrap: &Event,
    address: &PublicKey,
    conversation: &ConversationKey,
    verify_wrap_signature: bool,
) -> Result<OpenedStream, StreamError> {
    let wrap_kind = wrap.kind.as_u16();

    if wrap_kind != KIND_WRAP && wrap_kind != KIND_WRAP_EPHEMERAL {
        return Err(StreamError::BadWrapKind(wrap_kind));
    }

    if wrap.pubkey != *address {
        return Err(StreamError::WrongStream);
    }

    if verify_wrap_signature && wrap.verify().is_err() {
        return Err(StreamError::BadWrapSignature);
    }

    let seal: Event = Event::from_json(decode_content(conversation, &wrap.content)?)
        .map_err(|error| StreamError::Parse(error.to_string()))?;
    let seal_kind = seal.kind.as_u16();
    let seal_form = SealForm::from_kind(seal_kind).ok_or(StreamError::BadSealKind(seal_kind))?;
    seal.verify().map_err(|_| StreamError::BadSealSignature)?;

    let rumor_json = match seal_form {
        SealForm::Plaintext => seal.content.clone(),
        SealForm::Encrypted => decode_content(conversation, &seal.content)?,
    };

    let mut rumor: UnsignedEvent = UnsignedEvent::from_json(rumor_json.as_bytes())
        .map_err(|error| StreamError::Parse(error.to_string()))?;

    if rumor.pubkey != seal.pubkey {
        return Err(StreamError::AuthorMismatch);
    }

    let computed = rumor.compute_id();
    if let Some(claimed) = rumor.id
        && claimed != computed
    {
        return Err(StreamError::BadRumorId);
    }
    rumor.id = Some(computed);

    let at_ms = resolve_ms_strict(&rumor)?;

    Ok(OpenedStream {
        rumor_id: computed,
        author: seal.pubkey,
        seal_form,
        seal,
        wrapper_id: wrap.id,
        at_ms,
        rumor,
    })
}

pub fn channel_binding_tags(channel: &ChannelId, epoch: Epoch) -> Vec<Tag> {
    vec![
        Tag::custom(TAG_CHANNEL, [channel.to_hex()]),
        Tag::custom(TAG_EPOCH, [epoch.0.to_string()]),
    ]
}

pub fn check_channel_binding(
    rumor: &UnsignedEvent,
    channel: &ChannelId,
    epoch: Epoch,
) -> Result<(), StreamError> {
    match unique_tag(rumor, TAG_CHANNEL)? {
        Some(value) if value == channel.to_hex() => {}
        Some(_) => return Err(StreamError::ChannelMismatch),
        None => return Err(StreamError::MissingTag(TAG_CHANNEL)),
    }

    match unique_tag(rumor, TAG_EPOCH)? {
        Some(value) if value == epoch.0.to_string() => {}
        Some(_) => return Err(StreamError::EpochMismatch),
        None => return Err(StreamError::MissingTag(TAG_EPOCH)),
    }

    Ok(())
}

fn encrypt(conversation: &ConversationKey, plaintext: &[u8]) -> Result<Vec<u8>, StreamError> {
    let mut nonce = [0u8; 32];

    crate::fill_random(&mut nonce).map_err(|error| StreamError::Encrypt(error.to_string()))?;

    encrypt_to_bytes_with_nonce(conversation, plaintext, nonce)
        .map_err(|error| StreamError::Encrypt(error.to_string()))
}

fn decode_content(conversation: &ConversationKey, content: &str) -> Result<String, StreamError> {
    let plaintext = open_bytes(conversation, content)?;

    String::from_utf8(plaintext).map_err(|error| StreamError::Parse(error.to_string()))
}

fn check_plaintext_cap(len: usize) -> Result<(), StreamError> {
    if len > NIP44_MAX_PLAINTEXT {
        return Err(StreamError::Oversize(len));
    }

    Ok(())
}

fn unique_tag(rumor: &UnsignedEvent, name: &'static str) -> Result<Option<String>, StreamError> {
    let mut found: Option<String> = None;

    for tag in rumor.tags.iter() {
        let fields = tag.as_slice();
        if fields.len() >= 2 && fields[0] == name {
            if found.is_some() {
                return Err(StreamError::DuplicateTag(name));
            }
            found = Some(fields[1].clone());
        }
    }

    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive::channel_group_key;

    const SECRET: [u8; 32] = [0x07u8; 32];
    const OTHER_SECRET: [u8; 32] = [0x08u8; 32];

    fn channel() -> ChannelId {
        ChannelId::from_bytes([0xabu8; 32])
    }

    fn group(epoch: u64) -> GroupKey {
        channel_group_key(&SECRET, &channel(), Epoch(epoch)).expect("derives")
    }

    fn wrapper_p_tag(wrap: &Event) -> Option<String> {
        wrap.tags
            .iter()
            .find(|tag| tag.as_slice().first().map(String::as_str) == Some("p"))
            .and_then(|tag| tag.as_slice().get(1).cloned())
    }

    fn bound_rumor(content: &str, author: PublicKey, at_ms: u64) -> UnsignedEvent {
        build_rumor_ms(
            9,
            author,
            content,
            channel_binding_tags(&channel(), Epoch(0)),
            at_ms,
        )
    }

    fn sealed(rumor: &UnsignedEvent, form: SealForm, author: &Keys) -> Event {
        smol::block_on(build_seal(rumor, form, &group(0), author)).expect("seals")
    }

    fn wrapped(seal: &Event, kind: u16, at_secs: u64) -> Event {
        wrap_seal(seal, &group(0), kind, Timestamp::from_secs(at_secs), &[])
            .expect("wraps")
            .0
    }

    fn encrypted_wrap(content: &str, author: &Keys, at_ms: u64, kind: u16) -> Event {
        let rumor = bound_rumor(content, author.public_key(), at_ms);
        wrapped(
            &sealed(&rumor, SealForm::Encrypted, author),
            kind,
            at_ms / 1000,
        )
    }

    #[test]
    fn both_seal_forms_round_trip() {
        let author = Keys::generate();
        let at_ms = 1_686_840_217_417;
        let wrap = encrypted_wrap("Hey chat!", &author, at_ms, KIND_WRAP);

        assert_eq!(wrap.kind, Kind::GiftWrap, "the durable wrap is kind 1059");
        assert_eq!(wrap.pubkey, group(0).pk(), "the stream key signs the wrap");

        let opened = open_wrap(&wrap, &group(0)).expect("opens");
        assert_eq!(opened.author, author.public_key());
        assert_eq!(opened.rumor.content, "Hey chat!");
        assert_eq!(opened.rumor_id, opened.rumor.id.expect("id is set"));
        assert_eq!(opened.wrapper_id, wrap.id);
        assert_eq!(opened.at_ms, at_ms);
        assert_eq!(opened.seal_form, SealForm::Encrypted);
        check_channel_binding(&opened.rumor, &channel(), Epoch(0)).expect("binding holds");

        // The wrap's `p` tag must identify neither the stream nor the author.
        let p = wrapper_p_tag(&wrap).expect("the wrap carries a p tag");
        assert_ne!(p, group(0).pk_hex());
        assert_ne!(p, author.public_key().to_hex());

        // Ephemeral actions ride the same structure at a kind relays must drop.
        let typing = encrypted_wrap("typing", &author, 5_000, KIND_WRAP_EPHEMERAL);
        assert_eq!(typing.kind.as_u16(), 21059);
        assert_eq!(
            open_wrap(&typing, &group(0)).expect("opens").rumor.content,
            "typing"
        );

        // The plaintext form carries the rumor's bytes verbatim, which is what
        // lets a compaction re-wrap the signed edition into a later epoch.
        let edition = build_rumor_secs(
            3308,
            author.public_key(),
            "an edition",
            vec![],
            1_700_000_000,
        );
        let seal = sealed(&edition, SealForm::Plaintext, &author);
        assert_eq!(seal.content, edition.as_json(), "the rumor rides verbatim");

        let opened = open_wrap(&wrapped(&seal, KIND_WRAP, 1), &group(0)).expect("opens");
        assert_eq!(opened.seal_form, SealForm::Plaintext);

        let (rewrapped, _) =
            rewrap_seal(&opened.seal, &group(1), &group(1), Timestamp::from_secs(2))
                .expect("rewraps");
        let reopened = open_wrap(&rewrapped, &group(1)).expect("opens");
        assert_eq!(reopened.rumor_id, opened.rumor_id, "the rumor id survives");
        assert_eq!(reopened.author, author.public_key());
        assert_eq!(
            reopened.seal.sig, opened.seal.sig,
            "the signature rides whole"
        );
        assert_ne!(reopened.wrapper_id, opened.wrapper_id);

        assert!(matches!(
            rewrap_seal(
                &sealed(&edition, SealForm::Encrypted, &author),
                &group(1),
                &group(1),
                Timestamp::from_secs(2)
            ),
            Err(StreamError::NotRewrappable)
        ));
    }

    #[test]
    fn hostile_wraps_are_dropped_in_order() {
        let author = Keys::generate();
        let impostor = Keys::generate();

        // Kind and address are settled before any decryption is attempted.
        let mut wrong_kind = encrypted_wrap("x", &author, 1_000, KIND_WRAP);
        wrong_kind.kind = Kind::Custom(1058);
        assert!(matches!(
            open_wrap(&wrong_kind, &group(0)),
            Err(StreamError::BadWrapKind(1058))
        ));

        let foreign = channel_group_key(&OTHER_SECRET, &channel(), Epoch(0)).expect("derives");
        let wrap = encrypted_wrap("x", &author, 1_000, KIND_WRAP);
        assert!(matches!(
            open_wrap(&wrap, &foreign),
            Err(StreamError::WrongStream)
        ));

        // A flipped ciphertext byte fails the NIP-44 MAC.
        let mut payload = BASE64
            .decode(wrap.content.as_bytes())
            .expect("content is base64");
        payload[40] ^= 0x01;
        let mut tampered = wrap.clone();
        tampered.content = BASE64.encode(&payload);
        assert!(matches!(
            open_wrap(&tampered, &group(0)),
            Err(StreamError::Decrypt(_))
        ));

        // A seal claiming an author it holds no signature for.
        let seal = sealed(
            &bound_rumor("spoof", author.public_key(), 1_000),
            SealForm::Encrypted,
            &impostor,
        );
        let mut swapped: serde_json::Value = serde_json::from_str(&seal.as_json()).expect("json");
        swapped["pubkey"] = serde_json::Value::String(author.public_key().to_hex());
        let seal = Event::from_json(swapped.to_string()).expect("a swapped pubkey still parses");
        assert!(matches!(
            open_wrap(&wrapped(&seal, KIND_WRAP, 1), &group(0)),
            Err(StreamError::BadSealSignature)
        ));

        // A seal that does not vouch for the rumor's author.
        let seal = sealed(
            &bound_rumor("spoof", impostor.public_key(), 1_000),
            SealForm::Encrypted,
            &author,
        );
        assert!(matches!(
            open_wrap(&wrapped(&seal, KIND_WRAP, 1), &group(0)),
            Err(StreamError::AuthorMismatch)
        ));

        // A claimed id the rumor's own bytes do not hash to. The plaintext seal
        // smuggles the forgery through verbatim.
        let rumor = bound_rumor("real", author.public_key(), 1_000);
        let mut forged: serde_json::Value = serde_json::from_str(&rumor.as_json()).expect("json");
        forged["id"] = serde_json::Value::String("00".repeat(32));
        let seal = EventBuilder::new(Kind::Custom(KIND_SEAL_PLAINTEXT), forged.to_string())
            .custom_created_at(rumor.created_at)
            .finalize(&author)
            .expect("seals");
        assert!(matches!(
            open_wrap(&wrapped(&seal, KIND_WRAP, 1), &group(0)),
            Err(StreamError::BadRumorId)
        ));

        // Binding splices: another channel, another epoch, a duplicate or none.
        let doubled = vec![channel_binding_tags(&channel(), Epoch(0)); 2].concat();
        let rumor = bound_rumor("x", author.public_key(), 1_000);

        assert!(matches!(
            check_channel_binding(&rumor, &ChannelId::from_bytes([0xcdu8; 32]), Epoch(0)),
            Err(StreamError::ChannelMismatch)
        ));

        assert!(matches!(
            check_channel_binding(&rumor, &channel(), Epoch(1)),
            Err(StreamError::EpochMismatch)
        ));

        let duplicate = build_rumor_ms(9, author.public_key(), "x", doubled, 1_000);
        assert!(matches!(
            check_channel_binding(&duplicate, &channel(), Epoch(0)),
            Err(StreamError::DuplicateTag(_))
        ));

        let unbound = build_rumor_ms(9, author.public_key(), "x", vec![], 1_000);
        assert!(matches!(
            check_channel_binding(&unbound, &channel(), Epoch(0)),
            Err(StreamError::MissingTag(_))
        ));

        let oversize = build_rumor_ms(
            9,
            author.public_key(),
            &"x".repeat(NIP44_MAX_PLAINTEXT + 1),
            vec![],
            1_000,
        );
        assert!(matches!(
            seal_content(&oversize, SealForm::Encrypted, &group(0)),
            Err(StreamError::Oversize(_))
        ));
    }

    #[test]
    fn ms_is_a_drop_gate() {
        let author = Keys::generate();

        let absent = build_rumor_secs(9, author.public_key(), "x", vec![], 1_000);
        assert_eq!(resolve_ms_strict(&absent).expect("resolves"), 1_000_000);

        let highest = build_rumor_ms(9, author.public_key(), "x", vec![], 1_000_999);
        assert_eq!(resolve_ms_strict(&highest).expect("resolves"), 1_000_999);

        for malformed in ["1000", "007", "abc", "+5", ""] {
            let rumor = build_rumor_secs(
                9,
                author.public_key(),
                "x",
                vec![Tag::custom(TAG_MS, [malformed.to_string()])],
                1_000,
            );
            assert!(
                matches!(resolve_ms_strict(&rumor), Err(StreamError::BadMs)),
                "{malformed:?} must be malformed"
            );
        }

        // Present but valueless is malformed, not an offset-0 default.
        let valueless = build_rumor_secs(
            9,
            author.public_key(),
            "x",
            vec![Tag::custom(TAG_MS, Vec::<String>::new())],
            1_000,
        );
        assert!(matches!(
            resolve_ms_strict(&valueless),
            Err(StreamError::BadMs)
        ));

        // A valued duplicate takes the first, matching Armada.
        let repeated = build_rumor_secs(
            9,
            author.public_key(),
            "x",
            vec![
                Tag::custom(TAG_MS, ["1".to_string()]),
                Tag::custom(TAG_MS, ["2".to_string()]),
            ],
            1_000,
        );
        assert_eq!(resolve_ms_strict(&repeated).expect("resolves"), 1_000_001);
    }
}
