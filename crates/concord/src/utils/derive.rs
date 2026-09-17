use anyhow::{Result, bail};
use hkdf::Hkdf;
use nostr::nips::nip44::v2::ConversationKey;
use nostr_sdk::prelude::{Keys, PublicKey, SecretKey};
use sha2::{Digest, Sha256};

use crate::{ChannelId, CommunityId, Epoch};

pub const TOKEN_LEN: usize = 16;

const LABEL_CHANNEL: &str = "concord/channel";
const LABEL_CONTROL: &str = "concord/control";
const LABEL_CONTROL_SIGNER: &str = "concord/control-signer";
const LABEL_REKEY_PSEUDONYM: &str = "concord/rekey-pseudonym";
const LABEL_BASE_REKEY_PSEUDONYM: &str = "concord/base-rekey-pseudonym";
const LABEL_RECIPIENT_PSEUDONYM: &str = "concord/recipient-pseudonym";
const LABEL_GUESTBOOK: &str = "concord/guestbook";
const LABEL_DISSOLVED: &str = "concord/dissolved";
const LABEL_GRANT: &str = "concord/grant";
const LABEL_BANLIST: &str = "concord/banlist";
const LABEL_PINS: &str = "concord/pins";
const LABEL_INVITE_LINKS: &str = "concord/invite-links";
const LABEL_INVITE_KEY: &str = "concord/invite-key";

const LABEL_COMMUNITY: &str = "concord/community";
const LABEL_EPOCH_COMMITMENT: &str = "concord/epoch-key-commitment";

const ZERO32: [u8; 32] = [0u8; 32];

fn build_info(label: &str, id32: &[u8; 32], epoch: Option<u64>) -> Vec<u8> {
    let mut info = Vec::with_capacity(label.len() + 1 + 32 + 8);
    info.extend_from_slice(label.as_bytes());
    info.push(0x00);
    info.extend_from_slice(id32);

    if let Some(epoch) = epoch {
        info.extend_from_slice(&epoch.to_be_bytes());
    }

    info
}

fn hkdf32(ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let mut okm = [0u8; 32];
    Hkdf::<Sha256>::new(None, ikm)
        .expand(info, &mut okm)
        .expect("expanding HKDF to 32 bytes is below the 255*32 ceiling");
    okm
}

fn hkdf_to_secret_key(ikm: &[u8], base_info: &[u8]) -> Result<SecretKey> {
    if let Ok(secret_key) = SecretKey::from_slice(&hkdf32(ikm, base_info)) {
        return Ok(secret_key);
    }

    for counter in 0u8..=u8::MAX {
        let mut info = Vec::with_capacity(base_info.len() + 1);
        info.extend_from_slice(base_info);
        info.push(counter);

        if let Ok(secret_key) = SecretKey::from_slice(&hkdf32(ikm, &info)) {
            return Ok(secret_key);
        }
    }

    bail!("seed stayed out of the secp256k1 scalar range across all 256 counters")
}

#[derive(Clone)]
pub struct GroupKey {
    keys: Keys,
    conversation: ConversationKey,
}

impl GroupKey {
    fn derive(label: &str, secret: &[u8], id32: &[u8; 32], epoch: Option<u64>) -> Result<Self> {
        let secret_key = hkdf_to_secret_key(secret, &build_info(label, id32, epoch))?;
        let keys = Keys::new(secret_key);
        let conversation = ConversationKey::derive(keys.secret_key(), &keys.public_key())?;

        Ok(Self { keys, conversation })
    }

    pub fn pk(&self) -> PublicKey {
        self.keys.public_key()
    }

    pub fn pk_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }

    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    pub fn conversation(&self) -> &ConversationKey {
        &self.conversation
    }
}

impl std::fmt::Debug for GroupKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupKey")
            .field("pk", &self.pk_hex())
            .finish()
    }
}

/// `secret` is the `community_root` for a public channel.
pub fn channel_group_key(secret: &[u8; 32], channel: &ChannelId, epoch: Epoch) -> Result<GroupKey> {
    GroupKey::derive(LABEL_CHANNEL, secret, channel.as_bytes(), Some(epoch.0))
}

/// The plane's read key: its conversation key encrypts the wraps for every member.
pub fn control_group_key(
    community_root: &[u8; 32],
    community_id: &CommunityId,
    epoch: Epoch,
) -> Result<GroupKey> {
    GroupKey::derive(
        LABEL_CONTROL,
        community_root,
        community_id.as_bytes(),
        Some(epoch.0),
    )
}

/// The plane's address and wrap signer, held only by staff; wraps still read under [`control_group_key`].
pub fn control_signer_group_key(
    control_root: &[u8; 32],
    community_id: &CommunityId,
    epoch: Epoch,
) -> Result<GroupKey> {
    GroupKey::derive(
        LABEL_CONTROL_SIGNER,
        control_root,
        community_id.as_bytes(),
        Some(epoch.0),
    )
}

/// Member-writable, unlike the Control Plane: a join or a leave is each member's own word.
pub fn guestbook_group_key(
    community_root: &[u8; 32],
    community_id: &CommunityId,
    epoch: Epoch,
) -> Result<GroupKey> {
    GroupKey::derive(
        LABEL_GUESTBOOK,
        community_root,
        community_id.as_bytes(),
        Some(epoch.0),
    )
}

/// Keyed by the prior `community_root`, so any retained member recovers any epoch's rekey.
pub fn channel_rekey_group_key(
    prior_root: &[u8; 32],
    channel: &ChannelId,
    new_epoch: Epoch,
) -> Result<GroupKey> {
    GroupKey::derive(
        LABEL_REKEY_PSEUDONYM,
        prior_root,
        channel.as_bytes(),
        Some(new_epoch.0),
    )
}

pub fn base_rekey_group_key(
    prior_root: &[u8; 32],
    community_id: &CommunityId,
    new_epoch: Epoch,
) -> Result<GroupKey> {
    GroupKey::derive(
        LABEL_BASE_REKEY_PSEUDONYM,
        prior_root,
        community_id.as_bytes(),
        Some(new_epoch.0),
    )
}

pub fn dissolved_group_key(community_id: &CommunityId) -> Result<GroupKey> {
    GroupKey::derive(LABEL_DISSOLVED, community_id.as_bytes(), &ZERO32, None)
}

pub fn community_id_of(owner_xonly: &[u8; 32], owner_salt: &[u8; 32]) -> CommunityId {
    let mut hasher = Sha256::new();
    hasher.update(LABEL_COMMUNITY.as_bytes());
    hasher.update(owner_xonly);
    hasher.update(owner_salt);
    CommunityId::from_bytes(hasher.finalize().into())
}

pub fn verify_community_id(
    community_id: &CommunityId,
    owner_xonly: &[u8; 32],
    owner_salt: &[u8; 32],
) -> bool {
    community_id_of(owner_xonly, owner_salt) == *community_id
}

/// The continuity a rekey blob must satisfy against the key currently held.
pub fn epoch_key_commitment(previous_epoch: Epoch, previous_key: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(LABEL_EPOCH_COMMITMENT.as_bytes());
    hasher.update(previous_epoch.0.to_be_bytes());
    hasher.update(previous_key);
    hasher.finalize().into()
}

/// Bound to the `community_id`, so a member's Grant coordinate survives every refounding.
pub fn grant_locator(community_id: &CommunityId, member_xonly: &[u8; 32]) -> [u8; 32] {
    hkdf32(
        community_id.as_bytes(),
        &build_info(LABEL_GRANT, member_xonly, None),
    )
}

pub fn banlist_locator(community_id: &CommunityId) -> [u8; 32] {
    hkdf32(
        community_id.as_bytes(),
        &build_info(LABEL_BANLIST, &ZERO32, None),
    )
}

pub fn pins_locator(community_id: &CommunityId, channel: &ChannelId) -> [u8; 32] {
    hkdf32(
        community_id.as_bytes(),
        &build_info(LABEL_PINS, channel.as_bytes(), None),
    )
}

/// Bound to the creator, so each creator owns exactly their own registry.
pub fn invite_links_locator(community_id: &CommunityId, creator_xonly: &[u8; 32]) -> [u8; 32] {
    hkdf32(
        community_id.as_bytes(),
        &build_info(LABEL_INVITE_LINKS, creator_xonly, None),
    )
}

/// Built from public inputs only, so a locator match proves nothing about authenticity
pub fn recipient_locator(
    rotator_xonly: &[u8; 32],
    recipient_xonly: &[u8; 32],
    scope_id: &[u8; 32],
    new_epoch: Epoch,
) -> [u8; 32] {
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(rotator_xonly);
    ikm[32..].copy_from_slice(recipient_xonly);
    hkdf32(
        &ikm,
        &build_info(LABEL_RECIPIENT_PSEUDONYM, scope_id, Some(new_epoch.0)),
    )
}

/// The raw output is the NIP-44 conversation key (CORD-05 §2).
pub fn invite_bundle_key(token: &[u8; TOKEN_LEN]) -> [u8; 32] {
    hkdf32(token, &build_info(LABEL_INVITE_KEY, &ZERO32, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL_E0_SEED: &str =
        "1a99a5958bf9fcc5336e6e19db42aabf36ffbfa12f38a1d5fbde2ae383ed751b";
    const CHANNEL_E0_PK: &str = "7a5c5dff759a63f1fc2779864487432bae3d1ea72c4ffabd39f4c1fdaf62097a";
    const CHANNEL_EMULTI_PK: &str =
        "f20c7d192cc87615d7341e86f38f85303f4708b40232d4fea521ab8217767391";
    const CONTROL_E0_PK: &str = "c43df20bf4d6eeaea5149619662ffe9b211f31e11bb4a59f56b6e906f702d46f";
    const CONTROL_SIGNER_E0_SEED: &str =
        "c4a3e8354d95137132087356412b67b53e025d127d45de45cff9ecf45b0c24f6";
    const CONTROL_SIGNER_E0_PK: &str =
        "718aef388257f3fd9f1bfae5cf2cbd0594a2ffc31adb5c1fe22c502c046acaee";
    const CONTROL_SIGNER_EMULTI_PK: &str =
        "e27235cc13be2f9ad65648e01ff2b63402846469c8638b5386c625688194ec7d";
    const GUESTBOOK_E0_PK: &str =
        "ad09de582026fa7a052db18bb5827fa24c15e929d59aadcc91efb8508f5368ad";
    const CHANNEL_REKEY_E1_PK: &str =
        "7c55cdb957e9db2b4800d687b2a07d3f7066b1a35824a1e86ba871f55e87e8b5";
    const BASE_REKEY_E1_PK: &str =
        "fb2fa44fba66ba15595f784255a1cb569531db8784432ac0e4fe838498dd9dea";
    const DISSOLVED_PK: &str = "4d3d55d88fdf9d9c2089651e5cbb0dfa93b6b9b10cdcb2319b0dce1a1398096a";
    const GRANT_LOCATOR: &str = "fd2f88cc7f1eb8d7d862c91dc22afe700c358d1845158b3f353b769ce4898e35";
    const BANLIST_LOCATOR: &str =
        "88089214afae6d3c412fd817ada44d6df4d485a53565646471e74476397693c9";
    const INVITE_LINKS_LOCATOR: &str =
        "f4ae29994165767bac23e8dce630f81b926d2c8aa150e5cbf0bdf75865e8379a";
    const RECIPIENT_LOCATOR: &str =
        "342deb400e191f0f52c81f27600934552550beb85aa9bf169f02d0e7f826cf74";
    const INVITE_KEY: &str = "94bf8b0d89e579ddaeccf8d9db3f5de5c86a1259c597f2560ff0120173bc5e1f";
    const COMMUNITY_ID: &str = "2b790bd59df98bdc52092b74ebd6933a89ef8eaeecc9030861cbdeae7c814c46";
    const EPOCH_COMMITMENT: &str =
        "3e6d6a3c9973c16d1ca7c5602d36979927c55c21a7e2c840f883af3f047e80a4";
    const PINS_LOCATOR: &str = "3b4529395a35c981ed409b588af3c4cd3081992958a485347356a173c3146c52";
    const EPOCH_MULTI: u64 = 0x0102030405060708;

    /// `0x00..0x1f` / `0xff..0xe0` / `0x11` x32 — the inputs every vector uses.
    fn secret() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (index, byte) in key.iter_mut().enumerate() {
            *byte = index as u8;
        }
        key
    }

    fn id32() -> [u8; 32] {
        let mut id = [0u8; 32];
        for (index, byte) in id.iter_mut().enumerate() {
            *byte = 255 - index as u8;
        }
        id
    }

    fn hex(bytes: &[u8]) -> String {
        data_encoding::HEXLOWER.encode(bytes)
    }

    #[test]
    fn golden_vectors() {
        let secret = secret();
        let id = id32();
        let alt = [0x11u8; 32];
        let community_id = CommunityId::from_bytes(id);
        let channel = ChannelId::from_bytes(id);

        let channel_e0 = channel_group_key(&secret, &channel, Epoch(0)).expect("derives");
        assert_eq!(
            hex(channel_e0.keys().secret_key().as_secret_bytes()),
            CHANNEL_E0_SEED
        );
        assert_eq!(channel_e0.pk_hex(), CHANNEL_E0_PK);
        assert_eq!(
            channel_group_key(&secret, &channel, Epoch(EPOCH_MULTI))
                .expect("derives")
                .pk_hex(),
            CHANNEL_EMULTI_PK
        );

        assert_eq!(
            control_group_key(&secret, &community_id, Epoch(0))
                .expect("derives")
                .pk_hex(),
            CONTROL_E0_PK
        );

        let signer = control_signer_group_key(&secret, &community_id, Epoch(0)).expect("derives");
        assert_eq!(
            hex(signer.keys().secret_key().as_secret_bytes()),
            CONTROL_SIGNER_E0_SEED
        );
        assert_eq!(signer.pk_hex(), CONTROL_SIGNER_E0_PK);
        assert_eq!(
            control_signer_group_key(&secret, &community_id, Epoch(EPOCH_MULTI))
                .expect("derives")
                .pk_hex(),
            CONTROL_SIGNER_EMULTI_PK
        );

        assert_eq!(
            guestbook_group_key(&secret, &community_id, Epoch(0))
                .expect("derives")
                .pk_hex(),
            GUESTBOOK_E0_PK
        );

        assert_eq!(
            channel_rekey_group_key(&secret, &channel, Epoch(1))
                .expect("derives")
                .pk_hex(),
            CHANNEL_REKEY_E1_PK
        );
        assert_eq!(
            base_rekey_group_key(&secret, &community_id, Epoch(1))
                .expect("derives")
                .pk_hex(),
            BASE_REKEY_E1_PK
        );
        assert_eq!(
            dissolved_group_key(&community_id)
                .expect("derives")
                .pk_hex(),
            DISSOLVED_PK
        );

        assert_eq!(hex(&grant_locator(&community_id, &alt)), GRANT_LOCATOR);
        assert_eq!(hex(&banlist_locator(&community_id)), BANLIST_LOCATOR);
        assert_eq!(
            hex(&invite_links_locator(&community_id, &alt)),
            INVITE_LINKS_LOCATOR
        );
        assert_eq!(hex(&pins_locator(&community_id, &channel)), PINS_LOCATOR);
        assert_eq!(
            hex(&recipient_locator(&secret, &alt, &id, Epoch(3))),
            RECIPIENT_LOCATOR
        );
        assert_eq!(hex(&invite_bundle_key(&[0x07u8; TOKEN_LEN])), INVITE_KEY);

        assert_eq!(hex(community_id_of(&secret, &alt).as_bytes()), COMMUNITY_ID);
        assert_eq!(
            hex(&epoch_key_commitment(Epoch(2), &secret)),
            EPOCH_COMMITMENT
        );
    }
}
