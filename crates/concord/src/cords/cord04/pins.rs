use std::fmt;

use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use data_encoding::{BASE64, HEXLOWER};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::cord01::{self, OpenedStream, SealForm, resolve_ms_strict};
use crate::cord03::{ChatAction, ChatRumor, KIND_COMMENT, KIND_EDIT, KIND_MESSAGE};
use crate::cord04::canonical_decimal;
use crate::{ChannelId, Epoch, Extra, GroupKey, decode_hex_lower};

pub const PIN_MAX_ENTRIES: usize = 25;
pub const PIN_MAX_CONTENT_BYTES: usize = 32_768;

/// The serialized disclosure: `chacha_key[32] || chacha_nonce[12] || hmac_key[32]`.
pub const MESSAGE_KEYS_BYTES: usize = 76;

const TAG_CHANNEL: &str = "channel";
const TAG_EPOCH: &str = "epoch";
const TAG_TARGET: &str = "e";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    NotEncryptedSeal,
    BadPayload,
    Unverifiable,
    Unreadable,
    TooManyEntries,
    Oversize(usize),
    Seal(String),
    Encode(String),
}

impl fmt::Display for PinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PinError::NotEncryptedSeal => write!(f, "pin requires an encrypted seal"),
            PinError::BadPayload => write!(f, "the seal payload does not open"),
            PinError::Unverifiable => write!(f, "the entry would not verify"),
            PinError::Unreadable => {
                write!(f, "refusing to publish a pin list this client cannot read")
            }
            PinError::TooManyEntries => write!(f, "pin list exceeds {PIN_MAX_ENTRIES} entries"),
            PinError::Oversize(len) => {
                write!(
                    f,
                    "pin list content is {len} bytes (cap {PIN_MAX_CONTENT_BYTES})"
                )
            }
            PinError::Seal(error) => write!(f, "seal: {error}"),
            PinError::Encode(error) => write!(f, "encode: {error}"),
        }
    }
}

impl std::error::Error for PinError {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MessageKeys {
    chacha_key: [u8; 32],
    chacha_nonce: [u8; 12],
    hmac_key: [u8; 32],
}

impl MessageKeys {
    pub fn to_hex(&self) -> String {
        let mut packed = [0u8; MESSAGE_KEYS_BYTES];
        packed[0..32].copy_from_slice(&self.chacha_key);
        packed[32..44].copy_from_slice(&self.chacha_nonce);
        packed[44..76].copy_from_slice(&self.hmac_key);
        HEXLOWER.encode(&packed)
    }

    pub fn from_hex(value: &str) -> Option<Self> {
        let bytes = decode_hex_lower::<MESSAGE_KEYS_BYTES>(value).ok()?;

        Some(Self {
            chacha_key: bytes[0..32].try_into().ok()?,
            chacha_nonce: bytes[32..44].try_into().ok()?,
            hmac_key: bytes[44..76].try_into().ok()?,
        })
    }

    fn derive(conversation_key: &[u8; 32], nonce: &[u8]) -> Option<Self> {
        let hkdf = Hkdf::<Sha256>::from_prk(conversation_key).ok()?;
        let mut key_material = [0u8; MESSAGE_KEYS_BYTES];
        hkdf.expand(nonce, &mut key_material).ok()?;

        Some(Self {
            chacha_key: key_material[0..32].try_into().ok()?,
            chacha_nonce: key_material[32..44].try_into().ok()?,
            hmac_key: key_material[44..76].try_into().ok()?,
        })
    }
}

struct Payload {
    nonce: [u8; 32],
    ciphertext: Vec<u8>,
    mac: [u8; 32],
}

fn decode_payload(payload: &str) -> Option<Payload> {
    let data = BASE64.decode(payload.as_bytes()).ok()?;

    if data.len() < 99 || data[0] != 2 {
        return None;
    }

    let mac_at = data.len() - 32;

    Some(Payload {
        nonce: data[1..33].try_into().ok()?,
        ciphertext: data[33..mac_at].to_vec(),
        mac: data[mac_at..].try_into().ok()?,
    })
}

fn disclose_keys(payload: &str, conversation_key: &[u8; 32]) -> Option<MessageKeys> {
    let decoded = decode_payload(payload)?;
    MessageKeys::derive(conversation_key, &decoded.nonce)
}

fn open_payload(payload: &str, keys: &MessageKeys) -> Option<String> {
    let decoded = decode_payload(payload)?;

    let mut mac = Hmac::<Sha256>::new_from_slice(&keys.hmac_key).ok()?;
    mac.update(&decoded.nonce);
    mac.update(&decoded.ciphertext);
    mac.verify_slice(&decoded.mac).ok()?;

    let mut padded = decoded.ciphertext;
    let mut cipher = ChaCha20::new((&keys.chacha_key).into(), (&keys.chacha_nonce).into());
    cipher.apply_keystream(&mut padded);

    unpad(&padded)
}

fn unpad(padded: &[u8]) -> Option<String> {
    let (len, prefix) = plaintext_length(padded)?;
    let unpadded = padded.get(prefix..prefix.checked_add(len)?)?;

    if len < 1 || padded.len() != prefix.checked_add(padded_len(len)?)? {
        return None;
    }

    String::from_utf8(unpadded.to_vec()).ok()
}

fn plaintext_length(padded: &[u8]) -> Option<(usize, usize)> {
    let short = u16::from_be_bytes(padded.get(..2)?.try_into().ok()?);

    if short != 0 {
        return Some((short as usize, 2));
    }

    let long = u32::from_be_bytes(padded.get(2..6)?.try_into().ok()?);

    if long < 65_536 {
        return None;
    }

    Some((long as usize, 6))
}

fn padded_len(len: usize) -> Option<usize> {
    if len < 1 {
        return None;
    }

    if len <= 32 {
        return Some(32);
    }

    let next_power = 1usize.checked_shl(usize::BITS - (len - 1).leading_zeros())?;
    let chunk = if next_power <= 256 {
        32
    } else {
        next_power / 8
    };

    Some(chunk * ((len - 1) / chunk + 1))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinEditBundle {
    pub seal: Event,
    pub keys: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinEntry {
    pub seal: Event,
    pub keys: String,
    /// An unverifiable locator hint; a mismatch is expected and never fatal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<PinEditBundle>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditedContent {
    pub content: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct VerifiedPin {
    pub rumor_id: EventId,
    pub author: PublicKey,
    pub kind: u16,
    pub content: String,
    pub tags: Tags,
    pub epoch: Epoch,
    pub at_ms: u64,
    pub created_at: u64,
    pub wrap: Option<String>,
    pub edited: Option<EditedContent>,
    pub entry: PinEntry,
}

#[derive(Debug, Clone, Default)]
pub struct ReadPinList {
    pub entries: Vec<PinEntry>,
    pub sealed: bool,
}

pub fn build_entry(
    opened: &OpenedStream,
    group: &GroupKey,
    channel: &ChannelId,
) -> Result<PinEntry, PinError> {
    let keys = disclosed_keys(opened, group)?;

    let entry = PinEntry {
        seal: opened.seal.clone(),
        keys: keys.to_hex(),
        wrap: Some(opened.wrapper_id.to_hex()),
        edit: None,
        extra: Extra::default(),
    };

    if verify_entry(&entry, channel).is_none() {
        return Err(PinError::Unverifiable);
    }

    Ok(entry)
}

pub fn build_edit_bundle(
    edit: &OpenedStream,
    group: &GroupKey,
    original: &VerifiedPin,
    channel: &ChannelId,
) -> Result<PinEditBundle, PinError> {
    let bundle = PinEditBundle {
        seal: edit.seal.clone(),
        keys: disclosed_keys(edit, group)?.to_hex(),
    };

    if verify_edit_bundle(&bundle, &original.author, &original.rumor_id, channel).is_none() {
        return Err(PinError::Unverifiable);
    }

    Ok(bundle)
}

pub fn with_proven_edit(
    entry: &PinEntry,
    edit: &OpenedStream,
    group: &GroupKey,
    channel: &ChannelId,
) -> PinEntry {
    let Some(original) = verify_entry(entry, channel) else {
        return entry.clone();
    };

    let Ok(bundle) = build_edit_bundle(edit, group, &original, channel) else {
        return entry.clone();
    };

    let mut refreshed = entry.clone();
    refreshed.edit = Some(bundle);
    refreshed
}

fn disclosed_keys(opened: &OpenedStream, group: &GroupKey) -> Result<MessageKeys, PinError> {
    if opened.seal_form != SealForm::Encrypted {
        return Err(PinError::NotEncryptedSeal);
    }

    let conversation: [u8; 32] = group
        .conversation()
        .as_bytes()
        .try_into()
        .map_err(|_| PinError::BadPayload)?;

    let keys = disclose_keys(&opened.seal.content, &conversation).ok_or(PinError::BadPayload)?;

    if open_payload(&opened.seal.content, &keys).is_none() {
        return Err(PinError::BadPayload);
    }

    Ok(keys)
}

pub fn verify_entry(entry: &PinEntry, channel: &ChannelId) -> Option<VerifiedPin> {
    let seal = &entry.seal;

    if seal.kind.as_u16() != cord01::KIND_SEAL_ENCRYPTED || seal.verify().is_err() {
        return None;
    }

    let keys = MessageKeys::from_hex(&entry.keys)?;
    let plaintext = open_payload(&seal.content, &keys)?;
    let rumor = UnsignedEvent::from_json(&plaintext).ok()?;

    // NIP-59's impersonation check: the renderer shows the rumor's fields.
    if rumor.pubkey != seal.pubkey {
        return None;
    }

    let kind = rumor.kind.as_u16();

    if kind != KIND_MESSAGE && kind != KIND_COMMENT {
        return None;
    }

    // CORD-01's binding, restated: a keyholder must not pin a message into another Channel's list.
    if tag_value(&rumor, TAG_CHANNEL)? != channel.to_hex() {
        return None;
    }

    let epoch = Epoch(canonical_decimal(tag_value(&rumor, TAG_EPOCH)?)?);

    // Recomputed from the decrypted bytes; a claimed `id` is never trusted.
    rumor.verify_id().ok()?;
    let rumor_id = rumor.compute_id();

    let edited = entry
        .edit
        .as_ref()
        .and_then(|bundle| verify_edit_bundle(bundle, &rumor.pubkey, &rumor_id, channel));

    Some(VerifiedPin {
        author: rumor.pubkey,
        content: edited
            .as_ref()
            .map_or_else(|| rumor.content.clone(), |edited| edited.content.clone()),
        epoch,
        at_ms: resolve_ms_strict(&rumor).ok()?,
        created_at: rumor.created_at.as_secs(),
        tags: rumor.tags.clone(),
        wrap: entry.wrap.clone(),
        edited,
        entry: entry.clone(),
        kind,
        rumor_id,
    })
}

fn verify_edit_bundle(
    bundle: &PinEditBundle,
    original_author: &PublicKey,
    original_id: &EventId,
    channel: &ChannelId,
) -> Option<EditedContent> {
    let seal = &bundle.seal;

    // Checkable before any crypto: nobody else may revise another member's words.
    if seal.kind.as_u16() != cord01::KIND_SEAL_ENCRYPTED || seal.pubkey != *original_author {
        return None;
    }

    if seal.verify().is_err() {
        return None;
    }

    let keys = MessageKeys::from_hex(&bundle.keys)?;
    let plaintext = open_payload(&seal.content, &keys)?;
    let rumor = UnsignedEvent::from_json(&plaintext).ok()?;

    if rumor.pubkey != seal.pubkey || rumor.kind.as_u16() != KIND_EDIT {
        return None;
    }

    if tag_value(&rumor, TAG_CHANNEL)? != channel.to_hex() {
        return None;
    }

    if tag_value(&rumor, TAG_TARGET)? != original_id.to_hex() {
        return None;
    }

    rumor.verify_id().ok()?;

    Some(EditedContent {
        content: rumor.content.clone(),
        at_ms: resolve_ms_strict(&rumor).ok()?,
    })
}

fn tag_value<'a>(rumor: &'a UnsignedEvent, name: &str) -> Option<&'a str> {
    rumor
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(String::as_str) == Some(name))
        .and_then(|tag| tag.as_slice().get(1))
        .map(String::as_str)
}

#[derive(Serialize, Deserialize)]
struct PlainForm {
    entries: Vec<PinEntry>,
}

pub fn publishable(
    read: &ReadPinList,
    private: bool,
    group: &GroupKey,
    epoch: Epoch,
) -> Result<String, PinError> {
    if read.sealed {
        return Err(PinError::Unreadable);
    }

    if private {
        serialize_sealed(&read.entries, group, epoch)
    } else {
        serialize_public(&read.entries)
    }
}

fn serialize_public(entries: &[PinEntry]) -> Result<String, PinError> {
    let content = encode_form(entries)?;
    check_caps(entries.len(), &content)?;

    Ok(content)
}

fn serialize_sealed(
    entries: &[PinEntry],
    group: &GroupKey,
    epoch: Epoch,
) -> Result<String, PinError> {
    if entries.len() > PIN_MAX_ENTRIES {
        return Err(PinError::TooManyEntries);
    }

    let inner = encode_form(entries)?;
    let sealed = cord01::seal_bytes(group.conversation(), inner.as_bytes())
        .map_err(|error| PinError::Seal(error.to_string()))?;
    let content = serde_json::json!({ "epoch": epoch.to_string(), "sealed": sealed }).to_string();

    check_caps(entries.len(), &content)?;

    Ok(content)
}

fn encode_form(entries: &[PinEntry]) -> Result<String, PinError> {
    serde_json::to_string(&PlainForm {
        entries: entries.to_vec(),
    })
    .map_err(|error| PinError::Encode(error.to_string()))
}

fn check_caps(count: usize, content: &str) -> Result<(), PinError> {
    if count > PIN_MAX_ENTRIES {
        return Err(PinError::TooManyEntries);
    }

    if content.len() > PIN_MAX_CONTENT_BYTES {
        return Err(PinError::Oversize(content.len()));
    }

    Ok(())
}

pub fn read_list(content: &str, unseal: impl Fn(Epoch) -> Option<GroupKey>) -> ReadPinList {
    const EMPTY: ReadPinList = ReadPinList {
        entries: Vec::new(),
        sealed: false,
    };

    if content.len() > PIN_MAX_CONTENT_BYTES {
        return EMPTY;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return EMPTY;
    };

    if value.get("entries").is_some() {
        return match serde_json::from_value::<PlainForm>(value) {
            Ok(form) if form.entries.len() <= PIN_MAX_ENTRIES => ReadPinList {
                entries: form.entries,
                sealed: false,
            },
            _ => EMPTY,
        };
    }

    let (Some(epoch), Some(sealed)) = (
        value.get("epoch").and_then(serde_json::Value::as_str),
        value.get("sealed").and_then(serde_json::Value::as_str),
    ) else {
        return EMPTY;
    };

    let Some(epoch) = canonical_decimal(epoch) else {
        return EMPTY;
    };

    let Some(group) = unseal(Epoch(epoch)) else {
        return ReadPinList {
            sealed: true,
            ..EMPTY
        };
    };

    let Ok(inner) = cord01::open_bytes(group.conversation(), sealed) else {
        return EMPTY;
    };

    let Ok(form) = serde_json::from_slice::<PlainForm>(&inner) else {
        return EMPTY;
    };

    if form.entries.len() > PIN_MAX_ENTRIES {
        return EMPTY;
    }

    ReadPinList {
        entries: form.entries,
        sealed: false,
    }
}

pub fn killed_by(pin: &VerifiedPin, delete: &ChatRumor) -> bool {
    delete.author == pin.author
        && matches!(&delete.action, ChatAction::Delete { target, .. } if *target == pin.rumor_id)
}

#[cfg(test)]
mod tests {
    use nostr::nips::nip44::v2::{self, ConversationKey};

    use super::*;
    use crate::cord03::{ChatRumor, build_delete, build_edit, build_message, open, seal_rumor};
    use crate::derive::channel_group_key;

    const AT_MS: u64 = 1_700_000_000_000;
    const SECRET: [u8; 32] = [0x21u8; 32];

    fn channel() -> ChannelId {
        ChannelId::from_bytes([0xabu8; 32])
    }

    fn group() -> GroupKey {
        channel_group_key(&SECRET, &channel(), Epoch(0)).expect("derives")
    }

    fn conversation() -> ConversationKey {
        *group().conversation()
    }

    /// A real message through the production seal/open pipeline, as a pinner sees it.
    fn sealed_message(author: &Keys, text: &str, at_ms: u64) -> (OpenedStream, ChatRumor) {
        let rumor = build_message(
            author.public_key(),
            &channel(),
            Epoch(0),
            text,
            None,
            at_ms,
            None,
        );
        let (wrap, _) = smol::block_on(seal_rumor(&rumor, &group(), author, false)).expect("seals");

        open(&wrap, &group(), &channel(), Epoch(0)).expect("opens")
    }

    fn entry_for(author: &Keys, text: &str) -> (PinEntry, OpenedStream) {
        let (opened, _) = sealed_message(author, text, AT_MS);
        let entry = build_entry(&opened, &group(), &channel()).expect("builds");
        (entry, opened)
    }

    fn some(entries: Vec<PinEntry>) -> ReadPinList {
        ReadPinList {
            entries,
            sealed: false,
        }
    }

    /// The load-bearing primitive: the reproduction must open what nostr's own
    /// encryption produced, through the disclosure alone.
    #[test]
    fn a_disclosure_opens_its_message_and_nothing_else() {
        let nonce = [0x5au8; 32];
        let disclosure =
            MessageKeys::derive(conversation().as_bytes().try_into().expect("32"), &nonce)
                .expect("derives");

        for text in ["a", "hello world", &"padding boundary ".repeat(40)] {
            let raw = v2::encrypt_to_bytes_with_nonce(&conversation(), text.as_bytes(), nonce)
                .expect("encrypts");
            let payload = BASE64.encode(&raw);

            assert_eq!(open_payload(&payload, &disclosure).as_deref(), Some(text));
        }

        // Another nonce discloses different keys, which open nothing else.
        let other = v2::encrypt_to_bytes_with_nonce(&conversation(), b"second", [0x99u8; 32])
            .expect("encrypts");
        assert!(open_payload(&BASE64.encode(&other), &disclosure).is_none());

        let hex = disclosure.to_hex();
        assert_eq!(
            MessageKeys::from_hex(&hex).map(|keys| keys.to_hex()),
            Some(hex.clone())
        );
        assert!(MessageKeys::from_hex(&hex.to_uppercase()).is_none());
    }

    #[test]
    fn a_built_entry_proves_its_author_and_cannot_cross_channels() {
        let author = Keys::generate();
        let (entry, opened) = entry_for(&author, "pin me");
        let verified = verify_entry(&entry, &channel()).expect("verifies");

        assert_eq!(verified.author, author.public_key());
        assert_eq!(verified.content, "pin me");
        assert_eq!(verified.rumor_id, opened.rumor_id);
        assert_eq!(verified.at_ms, AT_MS);
        assert_eq!(verified.epoch, Epoch(0));

        // A keyholder must not be able to pin channel X's message into Y's list.
        let foreign = ChannelId::from_bytes([0xcdu8; 32]);
        assert!(verify_entry(&entry, &foreign).is_none());

        // Tampered keys and a re-signed seal both fail.
        let mut bad_keys = entry.clone();
        bad_keys.keys = format!("00{}", &entry.keys[2..]);
        assert!(verify_entry(&bad_keys, &channel()).is_none());

        let mut forged = entry.clone();
        forged.seal.pubkey = Keys::generate().public_key();
        assert!(verify_entry(&forged, &channel()).is_none());

        // A rumor carrying a claimed id that is not its own is refused.
        let plaintext = cord01::open_bytes(&conversation(), &opened.seal.content).expect("opens");
        let mut value: serde_json::Value = serde_json::from_slice(&plaintext).expect("json");
        value["id"] = serde_json::Value::String("00".repeat(32));

        let raw = v2::encrypt_to_bytes_with_nonce(
            &conversation(),
            value.to_string().as_bytes(),
            [0x11u8; 32],
        )
        .expect("encrypts");
        let content = BASE64.encode(&raw);
        let seal = EventBuilder::new(Kind::Custom(cord01::KIND_SEAL_ENCRYPTED), &content)
            .custom_created_at(opened.seal.created_at)
            .finalize(&author)
            .expect("signs");

        let lying = PinEntry {
            keys: disclose_keys(&content, conversation().as_bytes().try_into().expect("32"))
                .expect("discloses")
                .to_hex(),
            seal,
            wrap: None,
            edit: None,
            extra: Extra::default(),
        };
        assert!(verify_entry(&lying, &channel()).is_none());
    }

    #[test]
    fn a_proven_edit_replaces_the_words_and_a_stranger_cannot_revise() {
        let author = Keys::generate();
        let (entry, original) = entry_for(&author, "teh typo");

        let edit = build_edit(
            author.public_key(),
            &channel(),
            Epoch(0),
            original.rumor_id,
            "the typo, fixed",
            AT_MS + 5_000,
            None,
        );
        let (wrap, _) = smol::block_on(seal_rumor(&edit, &group(), &author, false)).expect("seals");
        let (edit_opened, _) = open(&wrap, &group(), &channel(), Epoch(0)).expect("opens");

        let refreshed = with_proven_edit(&entry, &edit_opened, &group(), &channel());
        let verified = verify_entry(&refreshed, &channel()).expect("verifies");
        assert_eq!(verified.content, "the typo, fixed");
        assert_eq!(verified.edited.expect("edited").at_ms, AT_MS + 5_000);

        // A stranger's edit of the same message never attaches.
        let stranger = Keys::generate();
        let hijack = build_edit(
            stranger.public_key(),
            &channel(),
            Epoch(0),
            original.rumor_id,
            "hijacked",
            AT_MS + 6_000,
            None,
        );
        let (wrap, _) =
            smol::block_on(seal_rumor(&hijack, &group(), &stranger, false)).expect("seals");
        let (hijack_opened, _) = open(&wrap, &group(), &channel(), Epoch(0)).expect("opens");

        let unchanged = with_proven_edit(&entry, &hijack_opened, &group(), &channel());
        assert!(unchanged.edit.is_none());
    }

    #[test]
    fn both_list_forms_round_trip_and_obey_their_caps() {
        let author = Keys::generate();
        let (entry, _) = entry_for(&author, "hello");

        let public =
            publishable(&some(vec![entry.clone()]), false, &group(), Epoch(0)).expect("publishes");
        let read = read_list(&public, |_| None);
        assert!(!read.sealed);
        assert_eq!(read.entries.len(), 1);
        assert!(verify_entry(&read.entries[0], &channel()).is_some());

        // A sealed list stays dark without its key, lights with it, and a wrong
        // key reads empty rather than panicking.
        let at_epoch_4 = channel_group_key(&SECRET, &channel(), Epoch(4)).expect("derives");
        let sealed = publishable(&some(vec![entry.clone()]), true, &at_epoch_4, Epoch(4))
            .expect("publishes");

        let dark = read_list(&sealed, |_| None);
        assert!(dark.sealed && dark.entries.is_empty());

        let lit = read_list(&sealed, |epoch| {
            (epoch == Epoch(4))
                .then(|| channel_group_key(&SECRET, &channel(), Epoch(4)).expect("derives"))
        });
        assert!(!lit.sealed);
        assert!(verify_entry(&lit.entries[0], &channel()).is_some());

        assert!(read_list(&sealed, |_| Some(group())).entries.is_empty());

        // 26 entries: the writer refuses, and a hand-built violating edition
        // reads as empty rather than forking the chain.
        let many = vec![entry; PIN_MAX_ENTRIES + 1];
        assert_eq!(
            publishable(&some(many.clone()), false, &group(), Epoch(0)),
            Err(PinError::TooManyEntries)
        );
        let violating = serde_json::json!({ "entries": many }).to_string();
        assert!(read_list(&violating, |_| None).entries.is_empty());

        // Garbage never panics and never reads as a list.
        for bad in [
            "",
            "not json",
            "[]",
            "42",
            r#"{"entries": 7}"#,
            r#"{"epoch":"04","sealed":"y"}"#,
        ] {
            let read = read_list(bad, |_| None);
            assert!(read.entries.is_empty() && !read.sealed, "{bad}");
        }
    }

    #[test]
    fn a_dark_list_is_never_reformed_and_only_the_author_kills_a_pin() {
        let author = Keys::generate();
        let (entry, _) = entry_for(&author, "delete me later");

        let dark = ReadPinList {
            entries: vec![entry.clone()],
            sealed: true,
        };
        assert_eq!(
            publishable(&dark, false, &group(), Epoch(0)),
            Err(PinError::Unreadable)
        );

        let verified = verify_entry(&entry, &channel()).expect("verifies");

        for author_keys in [&author, &Keys::generate()] {
            let delete = build_delete(
                author_keys.public_key(),
                &channel(),
                Epoch(0),
                verified.rumor_id,
                Some(KIND_MESSAGE),
                None,
                AT_MS + 1_000,
            );
            let (wrap, _) =
                smol::block_on(seal_rumor(&delete, &group(), author_keys, false)).expect("seals");
            let (_, rumor) = open(&wrap, &group(), &channel(), Epoch(0)).expect("opens");

            assert_eq!(
                killed_by(&verified, &rumor),
                author_keys.public_key() == author.public_key()
            );
        }
    }
}
