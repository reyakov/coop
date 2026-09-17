use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Result, bail};
use nostr_sdk::prelude::PublicKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::derive::{banlist_locator, grant_locator};
use crate::edition::{
    AuthorityCitation, EditionMeta, EntityHead, Floors, ParsedEdition, fold_head, vsk,
};
use crate::{ChannelId, CommunityId, Extra, RoleId, decode_hex_32};

pub const MAX_ROLES_PER_COMMUNITY: usize = 100;
pub const MAX_ROLES_PER_MEMBER: usize = 64;
pub const MAX_BANLIST: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct Permissions(pub u64);

impl Permissions {
    pub const BAN: u64 = 1 << 4;
    pub const CREATE_INVITE: u64 = 1 << 6;
    pub const KICK: u64 = 1 << 3;
    pub const MANAGE_CHANNELS: u64 = 1 << 1;
    pub const MANAGE_MESSAGES: u64 = 1 << 5;
    pub const MANAGE_METADATA: u64 = 1 << 2;
    pub const MANAGE_ROLES: u64 = 1 << 0;
    pub const MENTION_EVERYONE: u64 = 1 << 9;
    pub const PIN_MESSAGES: u64 = 1 << 11;
    pub const STAFF_MASK: u64 = Self::MANAGE_ROLES
        | Self::MANAGE_CHANNELS
        | Self::MANAGE_METADATA
        | Self::BAN
        | Self::CREATE_INVITE
        | Self::PIN_MESSAGES;
    pub const VIEW_AUDIT_LOG: u64 = 1 << 8;

    pub fn contains(self, bits: u64) -> bool {
        self.0 & bits == bits
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn is_staff(self) -> bool {
        self.0 & Self::STAFF_MASK != 0
    }
}

impl Serialize for Permissions {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Permissions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            String(String),
            Number(u64),
        }

        match Raw::deserialize(deserializer)? {
            Raw::Number(bits) => Ok(Self(bits)),
            Raw::String(bits) => {
                if bits.is_empty() || !bits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(serde::de::Error::custom(
                        "permissions must be a decimal string",
                    ));
                }

                bits.parse().map(Self).map_err(serde::de::Error::custom)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "channel_id")]
pub enum RoleScope {
    Server,
    Channel(ChannelId),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Role {
    pub role_id: RoleId,
    pub name: String,
    /// 0 belongs to the owner.
    pub position: u32,
    pub permissions: Permissions,
    pub scope: RoleScope,
    #[serde(default)]
    pub color: u32,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Role {
    pub fn parse(content: &str) -> Option<Self> {
        serde_json::from_str(content).ok()
    }

    pub fn to_content(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub member: PublicKey,
    /// Empty is a revoke.
    #[serde(default)]
    pub role_ids: Vec<RoleId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_wrap: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Grant {
    pub fn parse(content: &str) -> Option<Self> {
        serde_json::from_str(content).ok()
    }

    pub fn to_content(&self) -> Result<String> {
        if self.role_ids.len() > MAX_ROLES_PER_MEMBER {
            bail!("grant exceeds {MAX_ROLES_PER_MEMBER} roles");
        }

        Ok(serde_json::to_string(self)?)
    }
}

pub fn parse_banlist(content: &str) -> Option<Vec<PublicKey>> {
    let entries: Vec<String> = serde_json::from_str(content).ok()?;
    let mut banned = Vec::with_capacity(entries.len());

    for entry in &entries {
        banned.push(PublicKey::from_slice(&decode_hex_32(entry).ok()?).ok()?);
    }

    Some(banned)
}

/// The graph aggregated from the folded Role and Grant editions; not a wire document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommunityRoles {
    roles: BTreeMap<RoleId, Role>,
    grants: BTreeMap<PublicKey, Grant>,
}

impl CommunityRoles {
    pub fn role(&self, role_id: &RoleId) -> Option<&Role> {
        self.roles.get(role_id)
    }

    pub fn roles(&self) -> impl Iterator<Item = &Role> {
        self.roles.values()
    }

    pub fn grants(&self) -> impl Iterator<Item = &Grant> {
        self.grants.values()
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.grants.is_empty()
    }

    pub fn roles_of<'a>(&'a self, member: &PublicKey) -> impl Iterator<Item = &'a Role> + 'a {
        self.grants
            .get(member)
            .into_iter()
            .flat_map(|grant| grant.role_ids.iter())
            .filter_map(|role_id| self.roles.get(role_id))
    }

    pub fn effective_permissions(&self, member: &PublicKey) -> Permissions {
        self.roles_of(member)
            .fold(Permissions::default(), |total, role| {
                total.union(role.permissions)
            })
    }

    pub fn has_permission(&self, member: &PublicKey, bits: u64) -> bool {
        self.effective_permissions(member).contains(bits)
    }

    pub fn highest_position(&self, member: &PublicKey) -> Option<u32> {
        self.roles_of(member).map(|role| role.position).min()
    }

    pub fn is_authorized(&self, actor: &PublicKey, owner: &PublicKey, permission: u64) -> bool {
        actor == owner || self.has_permission(actor, permission)
    }

    /// Strictly outranks: equal cannot act on equal.
    pub fn outranks(&self, actor: &PublicKey, owner: &PublicKey, target_position: u32) -> bool {
        if actor == owner {
            return true;
        }

        match self.highest_position(actor) {
            Some(position) => position < target_position,
            None => false,
        }
    }

    pub fn can_act_on_position(
        &self,
        actor: &PublicKey,
        owner: &PublicKey,
        target_position: u32,
        permission: u64,
    ) -> bool {
        if actor == owner {
            return true;
        }

        self.has_permission(actor, permission) && self.outranks(actor, owner, target_position)
    }

    pub fn can_act_on_member(
        &self,
        actor: &PublicKey,
        owner: &PublicKey,
        target: &PublicKey,
        permission: u64,
    ) -> bool {
        if target == owner {
            return false;
        }

        self.can_act_on_position(
            actor,
            owner,
            self.highest_position(target).unwrap_or(u32::MAX),
            permission,
        )
    }

    pub fn is_staff(&self, member: &PublicKey, owner: &PublicKey) -> bool {
        member == owner || self.effective_permissions(member).is_staff()
    }

    fn cap_roles(&mut self) {
        if self.roles.len() <= MAX_ROLES_PER_COMMUNITY {
            return;
        }

        let Some(threshold) = self.roles.keys().nth(MAX_ROLES_PER_COMMUNITY).copied() else {
            return;
        };

        self.roles.split_off(&threshold);

        let roles = &self.roles;
        self.grants.retain(|_, grant| {
            grant.role_ids.retain(|role_id| roles.contains_key(role_id));
            !grant.role_ids.is_empty()
        });
    }
}

#[derive(Debug, Clone)]
pub enum AuthorityContent {
    Role(Role),
    Grant(Grant),
    Banlist(Vec<PublicKey>),
}

#[derive(Debug, Clone)]
pub struct AuthorityEdition {
    pub entity: [u8; 32],
    pub meta: EditionMeta,
    pub author: PublicKey,
    pub citation: Option<AuthorityCitation>,
    pub content: AuthorityContent,
}

impl AuthorityEdition {
    /// `None` for anything the fold should drop rather than repair.
    pub fn parse(edition: &ParsedEdition, community_id: &CommunityId) -> Option<Self> {
        let content = match edition.subkind.as_str() {
            vsk::ROLE => {
                let role = Role::parse(&edition.content)?;

                if *role.role_id.as_bytes() != edition.entity || role.position == 0 {
                    return None;
                }

                AuthorityContent::Role(role)
            }
            vsk::GRANT => {
                let mut grant = Grant::parse(&edition.content)?;

                if grant_locator(community_id, &grant.member.to_bytes()) != edition.entity {
                    return None;
                }

                grant.role_ids.truncate(MAX_ROLES_PER_MEMBER);

                AuthorityContent::Grant(grant)
            }
            vsk::BANLIST => {
                if edition.entity != banlist_locator(community_id) {
                    return None;
                }

                AuthorityContent::Banlist(parse_banlist(&edition.content)?)
            }
            _ => return None,
        };

        Some(Self {
            entity: edition.entity,
            meta: EditionMeta::from(edition),
            author: edition.author,
            citation: edition.citation,
            content,
        })
    }

    pub fn head(&self) -> EntityHead {
        EntityHead {
            entity: self.entity,
            version: self.meta.version,
            self_hash: self.meta.self_hash,
            rumor_id: self.meta.tiebreak_id,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Roster {
    pub roles: CommunityRoles,
    pub banned: BTreeSet<PublicKey>,
    /// The role, grant and banlist heads this fold settled.
    pub floors: Floors,
    pub gapped: bool,
}

pub fn fold_roster(
    owner: &PublicKey,
    community_id: &CommunityId,
    editions: &[AuthorityEdition],
    floors: &Floors,
    held_bans: &BTreeSet<PublicKey>,
) -> Roster {
    let banlist = banlist_locator(community_id);
    let mut role_candidates: BTreeMap<[u8; 32], Vec<&AuthorityEdition>> = BTreeMap::new();
    let mut grant_candidates: BTreeMap<[u8; 32], Vec<&AuthorityEdition>> = BTreeMap::new();
    let mut banlist_candidates: Vec<&AuthorityEdition> = Vec::new();

    for edition in editions {
        if edition.meta.version < floors.get(&edition.entity).map_or(0, |floor| floor.version) {
            continue;
        }

        match &edition.content {
            AuthorityContent::Role(_) => {
                role_candidates
                    .entry(edition.entity)
                    .or_default()
                    .push(edition);
            }
            AuthorityContent::Grant(_) => {
                grant_candidates
                    .entry(edition.entity)
                    .or_default()
                    .push(edition);
            }
            AuthorityContent::Banlist(_) if edition.entity == banlist => {
                banlist_candidates.push(edition);
            }
            AuthorityContent::Banlist(_) => {}
        }
    }

    for candidates in role_candidates
        .values_mut()
        .chain(grant_candidates.values_mut())
    {
        rank(candidates);
    }

    rank(&mut banlist_candidates);

    let mut gapped = false;
    for (entity, candidates) in role_candidates.iter().chain(&grant_candidates) {
        gapped |= entity_gapped(entity, candidates, floors);
    }
    gapped |= entity_gapped(&banlist, &banlist_candidates, floors);

    let preliminary = select_authorized(
        owner,
        community_id,
        &role_candidates,
        &grant_candidates,
        &BTreeSet::new(),
    );

    let (banned, banlist_head, banlist_gapped) = fold_banlist(
        owner,
        community_id,
        &banlist_candidates,
        &preliminary.roles,
        floors,
        held_bans,
    );
    gapped |= banlist_gapped;

    let mut selection = select_authorized(
        owner,
        community_id,
        &role_candidates,
        &grant_candidates,
        &banned,
    );
    selection.roles.cap_roles();

    let mut floors = selection.floors;
    if let Some(head) = banlist_head {
        floors.insert(head.entity, head);
    }

    Roster {
        roles: selection.roles,
        banned,
        floors,
        gapped,
    }
}

pub fn citation_ok(
    owner: &PublicKey,
    community_id: &CommunityId,
    author: &PublicKey,
    citation: Option<&AuthorityCitation>,
    floors: &Floors,
) -> bool {
    if author == owner {
        return true;
    }

    let Some(citation) = citation else {
        return false;
    };

    let grant = grant_locator(community_id, &author.to_bytes());

    if citation.entity != grant {
        return false;
    }

    match floors.get(&grant) {
        Some(head) if head.version > citation.version => true,
        Some(head) if head.version == citation.version => head.self_hash == citation.hash,
        _ => false,
    }
}

fn rank(candidates: &mut Vec<&AuthorityEdition>) {
    candidates.sort_by(|a, b| {
        b.meta
            .version
            .cmp(&a.meta.version)
            .then(a.meta.tiebreak_id.cmp(&b.meta.tiebreak_id))
    });
}

fn entity_gapped(entity: &[u8; 32], candidates: &[&AuthorityEdition], floors: &Floors) -> bool {
    let metas: Vec<EditionMeta> = candidates.iter().map(|candidate| candidate.meta).collect();

    fold_head(&metas, floors.get(entity)).gap
}

#[derive(Debug, Default)]
struct Selection {
    roles: CommunityRoles,
    floors: Floors,
}

fn select_authorized(
    owner: &PublicKey,
    community_id: &CommunityId,
    role_candidates: &BTreeMap<[u8; 32], Vec<&AuthorityEdition>>,
    grant_candidates: &BTreeMap<[u8; 32], Vec<&AuthorityEdition>>,
    excluded: &BTreeSet<PublicKey>,
) -> Selection {
    let fixpoint = Fixpoint {
        owner,
        community_id,
        excluded,
        roles: role_candidates,
        grants: grant_candidates,
    };

    let bound = 2 * (role_candidates.len() + grant_candidates.len()) + 8;
    let mut accepted = Selection::default();

    for _ in 0..bound {
        let next = fixpoint.pass(&accepted);

        if next.roles == accepted.roles {
            return next;
        }

        accepted = next;
    }

    accepted
}

struct Fixpoint<'a> {
    owner: &'a PublicKey,
    community_id: &'a CommunityId,
    excluded: &'a BTreeSet<PublicKey>,
    roles: &'a BTreeMap<[u8; 32], Vec<&'a AuthorityEdition>>,
    grants: &'a BTreeMap<[u8; 32], Vec<&'a AuthorityEdition>>,
}

impl Fixpoint<'_> {
    fn pass(&self, accepted: &Selection) -> Selection {
        let mut next = Selection::default();

        for candidates in self.roles.values() {
            self.select_role(candidates, accepted, &mut next);
        }

        for candidates in self.grants.values() {
            self.select_grant(candidates, accepted, &mut next);
        }

        next
    }

    fn select_role(
        &self,
        candidates: &[&AuthorityEdition],
        accepted: &Selection,
        next: &mut Selection,
    ) {
        let mut admissible: HashSet<[u8; 32]> = HashSet::new();
        let mut standing: Option<u32> = None;
        let mut end = candidates.len();

        while end > 0 {
            let version = candidates[end - 1].meta.version;
            let mut start = end;

            while start > 0 && candidates[start - 1].meta.version == version {
                start -= 1;
            }

            // One winner per version, so fork siblings cannot sidestep the gates.
            for candidate in &candidates[start..end] {
                let AuthorityContent::Role(role) = &candidate.content else {
                    continue;
                };

                if self.excluded.contains(&candidate.author) {
                    continue;
                }

                if !accepted.roles.can_act_on_position(
                    &candidate.author,
                    self.owner,
                    role.position,
                    Permissions::MANAGE_ROLES,
                ) {
                    continue;
                }

                if let Some(previous) = standing
                    && !accepted.roles.can_act_on_position(
                        &candidate.author,
                        self.owner,
                        previous,
                        Permissions::MANAGE_ROLES,
                    )
                {
                    continue;
                }

                // An unresolvable citation parks the edition; an absent one declared no floor to wait for.
                if candidate.citation.is_some()
                    && !citation_ok(
                        self.owner,
                        self.community_id,
                        &candidate.author,
                        candidate.citation.as_ref(),
                        &accepted.floors,
                    )
                {
                    continue;
                }

                admissible.insert(candidate.meta.self_hash);
                standing = Some(role.position);
                break;
            }

            end = start;
        }

        for candidate in candidates {
            let AuthorityContent::Role(role) = &candidate.content else {
                continue;
            };

            if !admissible.contains(&candidate.meta.self_hash) {
                continue;
            }

            next.roles.roles.insert(role.role_id, role.clone());
            next.floors.insert(candidate.entity, candidate.head());
            break;
        }
    }

    fn select_grant(
        &self,
        candidates: &[&AuthorityEdition],
        accepted: &Selection,
        next: &mut Selection,
    ) {
        for candidate in candidates {
            let AuthorityContent::Grant(grant) = &candidate.content else {
                continue;
            };

            if self.excluded.contains(&candidate.author) || self.excluded.contains(&grant.member) {
                continue;
            }

            let resolved: Vec<(&RoleId, u32)> = grant
                .role_ids
                .iter()
                .filter_map(|role_id| {
                    accepted
                        .roles
                        .role(role_id)
                        .map(|role| (role_id, role.position))
                })
                .collect();

            if resolved.is_empty() && !grant.role_ids.is_empty() {
                continue;
            }

            let cited = citation_ok(
                self.owner,
                self.community_id,
                &candidate.author,
                candidate.citation.as_ref(),
                &accepted.floors,
            );

            if !cited && candidate.citation.is_some() {
                continue;
            }

            if !cited && resolved.is_empty() {
                continue;
            }

            let ranks_every_role = resolved.iter().all(|(_, position)| {
                accepted.roles.can_act_on_position(
                    &candidate.author,
                    self.owner,
                    *position,
                    Permissions::MANAGE_ROLES,
                )
            });

            if !ranks_every_role
                || !accepted.roles.can_act_on_member(
                    &candidate.author,
                    self.owner,
                    &grant.member,
                    Permissions::MANAGE_ROLES,
                )
            {
                continue;
            }

            // A revoke still advances the floor; it just does not belong in the roster.
            next.floors.insert(candidate.entity, candidate.head());

            if !resolved.is_empty() {
                let mut resolved_grant = grant.clone();
                resolved_grant.role_ids = resolved.iter().map(|(role_id, _)| **role_id).collect();
                next.roles.grants.insert(grant.member, resolved_grant);
            }

            break;
        }
    }
}

fn fold_banlist(
    owner: &PublicKey,
    community_id: &CommunityId,
    candidates: &[&AuthorityEdition],
    roster: &CommunityRoles,
    floors: &Floors,
    held_bans: &BTreeSet<PublicKey>,
) -> (BTreeSet<PublicKey>, Option<EntityHead>, bool) {
    let authorized: Vec<&AuthorityEdition> = candidates
        .iter()
        .copied()
        .filter(|candidate| {
            !held_bans.contains(&candidate.author)
                && roster.is_authorized(&candidate.author, owner, Permissions::BAN)
                && citation_ok(
                    owner,
                    community_id,
                    &candidate.author,
                    candidate.citation.as_ref(),
                    floors,
                )
        })
        .collect();

    if authorized.is_empty() {
        return (held_bans.clone(), None, false);
    }

    let entity = banlist_locator(community_id);
    let metas: Vec<EditionMeta> = authorized.iter().map(|candidate| candidate.meta).collect();
    let selection = fold_head(&metas, floors.get(&entity));

    let Some(index) = selection.head else {
        return (held_bans.clone(), None, selection.gap);
    };

    let head = authorized[index];

    let AuthorityContent::Banlist(entries) = &head.content else {
        return (held_bans.clone(), None, selection.gap);
    };

    let banned: BTreeSet<PublicKey> = entries
        .iter()
        .filter(|target| roster.can_act_on_member(&head.author, owner, target, Permissions::BAN))
        .take(MAX_BANLIST)
        .copied()
        .collect();

    (banned, Some(head.head()), selection.gap)
}

#[cfg(test)]
mod tests {
    use nostr_sdk::prelude::Keys;

    use super::*;
    use crate::edition::{EditionFields, build_edition, parse_edition};

    const COMMUNITY: [u8; 32] = [0xc0; 32];
    const AT: u64 = 1_700_000_000;

    fn community_id() -> CommunityId {
        CommunityId::from_bytes(COMMUNITY)
    }

    fn role(entity: [u8; 32], position: u32) -> Role {
        Role {
            role_id: RoleId::from_bytes(entity),
            name: "Mod".to_owned(),
            position,
            permissions: Permissions(Permissions::MANAGE_ROLES | Permissions::MANAGE_METADATA),
            scope: RoleScope::Server,
            color: 0,
            extra: Extra::default(),
        }
    }

    fn edition(
        author: &PublicKey,
        subkind: &str,
        entity: [u8; 32],
        content: &str,
        version: u64,
        citation: Option<AuthorityCitation>,
    ) -> AuthorityEdition {
        let rumor = build_edition(EditionFields {
            author: *author,
            subkind,
            entity,
            version,
            prev: None,
            citation,
            content,
            at_secs: AT,
        });

        AuthorityEdition::parse(&parse_edition(&rumor).expect("parses"), &community_id())
            .expect("recognizes")
    }

    fn role_edition(author: &PublicKey, id: u8, position: u32, version: u64) -> AuthorityEdition {
        let entity = [id; 32];
        let content = role(entity, position).to_content().expect("serializes");

        edition(author, vsk::ROLE, entity, &content, version, None)
    }

    fn grant_edition(
        author: &PublicKey,
        member: &PublicKey,
        roles: &[u8],
        version: u64,
        citation: Option<AuthorityCitation>,
    ) -> AuthorityEdition {
        let content = Grant {
            member: *member,
            role_ids: roles
                .iter()
                .map(|id| RoleId::from_bytes([*id; 32]))
                .collect(),
            control_wrap: None,
            extra: Extra::default(),
        }
        .to_content()
        .expect("serializes");

        edition(
            author,
            vsk::GRANT,
            grant_locator(&community_id(), &member.to_bytes()),
            &content,
            version,
            citation,
        )
    }

    fn fold(
        owner: &PublicKey,
        editions: &[AuthorityEdition],
        banned: &BTreeSet<PublicKey>,
    ) -> Roster {
        fold_roster(owner, &community_id(), editions, &Floors::new(), banned)
    }

    fn position(roster: &Roster, id: u8) -> Option<u32> {
        roster
            .roles
            .role(&RoleId::from_bytes([id; 32]))
            .map(|role| role.position)
    }

    #[test]
    fn permission_bits_are_frozen() {
        assert_eq!(Permissions::MANAGE_ROLES, 1);
        assert_eq!(Permissions::MANAGE_CHANNELS, 2);
        assert_eq!(Permissions::MANAGE_METADATA, 4);
        assert_eq!(Permissions::KICK, 8);
        assert_eq!(Permissions::BAN, 16);
        assert_eq!(Permissions::MANAGE_MESSAGES, 32);
        assert_eq!(Permissions::CREATE_INVITE, 64);
        assert_eq!(Permissions::VIEW_AUDIT_LOG, 256);
        assert_eq!(Permissions::MENTION_EVERYONE, 512);
        assert_eq!(Permissions::PIN_MESSAGES, 2048);
        assert_eq!(Permissions::STAFF_MASK, 1 | 2 | 4 | 16 | 64 | 2048);
    }

    #[test]
    fn role_content_round_trips_with_the_permissions_as_a_decimal_string() {
        let id = [0x01; 32];
        let content = role(id, 2).to_content().expect("serializes");

        assert!(
            content.contains("\"permissions\":\"5\""),
            "permissions ride as a string: {content}"
        );
        assert!(
            content.contains("\"scope\":{\"kind\":\"server\"}"),
            "{content}"
        );
        assert_eq!(Role::parse(&content).expect("parses").position, 2);

        let legacy = content.replace("\"permissions\":\"5\"", "\"permissions\":5");
        assert_eq!(
            Role::parse(&legacy).expect("parses").permissions,
            Permissions(Permissions::MANAGE_ROLES | Permissions::MANAGE_METADATA)
        );
        assert!(Role::parse(&content.replace("\"5\"", "\"+5\"")).is_none());

        let scoped = Role {
            scope: RoleScope::Channel(ChannelId::from_bytes([0x0a; 32])),
            ..role(id, 2)
        };
        assert!(
            scoped
                .to_content()
                .expect("serializes")
                .contains("\"scope\":{\"kind\":\"channel\",\"channel_id\":")
        );
    }

    #[test]
    fn authority_resolves_outward_from_the_owner_and_refuses_escalation() {
        let owner = Keys::generate();
        let admin = Keys::generate();
        let member = Keys::generate();
        let stranger = Keys::generate();

        let editions = vec![
            role_edition(&owner.public_key(), 0x01, 1, 1),
            grant_edition(&owner.public_key(), &admin.public_key(), &[0x01], 1, None),
            grant_edition(&owner.public_key(), &member.public_key(), &[0x01], 1, None),
        ];
        let roster = fold(&owner.public_key(), &editions, &BTreeSet::new());

        assert!(
            roster
                .roles
                .is_staff(&admin.public_key(), &owner.public_key())
        );
        assert!(
            roster
                .roles
                .is_staff(&member.public_key(), &owner.public_key())
        );
        assert!(
            !roster
                .roles
                .is_staff(&stranger.public_key(), &owner.public_key())
        );
        assert!(roster.roles.is_authorized(
            &owner.public_key(),
            &owner.public_key(),
            Permissions::MANAGE_METADATA
        ));

        // Equal cannot act on equal, and a roleless member outranks nobody.
        assert!(!roster.roles.can_act_on_position(
            &admin.public_key(),
            &owner.public_key(),
            1,
            Permissions::MANAGE_ROLES
        ));
        assert!(roster.roles.can_act_on_position(
            &admin.public_key(),
            &owner.public_key(),
            2,
            Permissions::MANAGE_ROLES
        ));
        assert!(!roster.roles.can_act_on_member(
            &admin.public_key(),
            &owner.public_key(),
            &member.public_key(),
            Permissions::BAN
        ));

        let banned = fold(
            &owner.public_key(),
            &editions,
            &BTreeSet::from([admin.public_key()]),
        );
        assert!(
            !banned
                .roles
                .is_staff(&admin.public_key(), &owner.public_key())
        );
        assert!(
            banned
                .roles
                .is_staff(&member.public_key(), &owner.public_key())
        );

        // An unauthorized higher version is dropped, not allowed to vanish the entity.
        let roster = fold(
            &owner.public_key(),
            &[
                role_edition(&stranger.public_key(), 0x02, 9, 9),
                role_edition(&owner.public_key(), 0x02, 3, 1),
            ],
            &BTreeSet::new(),
        );
        assert_eq!(position(&roster, 0x02), Some(3));

        // Nor may an admin republish a role out from under its holders.
        let roster = fold(
            &owner.public_key(),
            &[
                role_edition(&admin.public_key(), 0x03, 9, 2),
                role_edition(&owner.public_key(), 0x03, 1, 1),
            ],
            &BTreeSet::new(),
        );
        assert_eq!(position(&roster, 0x03), Some(1));
    }

    #[test]
    fn an_uncited_revocation_strips_nothing_while_a_cited_one_does() {
        let owner = Keys::generate();
        let moderator = Keys::generate();
        let member = Keys::generate();

        let editions = vec![
            role_edition(&owner.public_key(), 0x01, 5, 1),
            role_edition(&owner.public_key(), 0x02, 9, 1),
            grant_edition(
                &owner.public_key(),
                &moderator.public_key(),
                &[0x01],
                1,
                None,
            ),
            grant_edition(&owner.public_key(), &member.public_key(), &[0x02], 1, None),
        ];

        let roster = fold(&owner.public_key(), &editions, &BTreeSet::new());
        assert!(roster.roles.roles_of(&member.public_key()).next().is_some());
        assert!(roster.roles.can_act_on_member(
            &moderator.public_key(),
            &owner.public_key(),
            &member.public_key(),
            Permissions::MANAGE_ROLES
        ));

        let revoke = |citation| {
            grant_edition(
                &moderator.public_key(),
                &member.public_key(),
                &[],
                2,
                citation,
            )
        };

        // A revoke names no position to rank-check. Uncited it is only a stranger's
        // word, and strips nothing.
        let stripped = fold(
            &owner.public_key(),
            &[editions.clone(), vec![revoke(None)]].concat(),
            &BTreeSet::new(),
        );
        assert!(
            stripped
                .roles
                .roles_of(&member.public_key())
                .next()
                .is_some()
        );

        // Cited against the grant it acts under, the same revoke lands.
        let head = roster
            .floors
            .get(&grant_locator(
                &community_id(),
                &moderator.public_key().to_bytes(),
            ))
            .expect("the moderator's own grant folded");
        let stripped = fold(
            &owner.public_key(),
            &[
                editions,
                vec![revoke(Some(AuthorityCitation {
                    entity: head.entity,
                    version: head.version,
                    hash: head.self_hash,
                }))],
            ]
            .concat(),
            &BTreeSet::new(),
        );
        assert!(
            stripped
                .roles
                .roles_of(&member.public_key())
                .next()
                .is_none()
        );
        assert!(
            stripped
                .roles
                .is_staff(&moderator.public_key(), &owner.public_key())
        );
    }
}
