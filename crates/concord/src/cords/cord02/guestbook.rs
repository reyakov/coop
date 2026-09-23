use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use data_encoding::HEXLOWER;
use nostr_sdk::prelude::*;

use crate::cord01::{
    KIND_WRAP, OpenedStream, SealForm, build_rumor_ms, build_seal, open_wrap, wrap_seal,
};
use crate::cord04::{AuthorityCitation, canonical_decimal, citation_tag};
pub use crate::cords::rumor::RumorError as GuestbookError;
use crate::cords::rumor::{optional_citation, pubkey, required, value};
use crate::{GroupKey, decode_hex_32};

pub const KIND_JOIN_LEAVE: u16 = 3306;
pub const KIND_KICK: u16 = 3309;
pub const KIND_SNAPSHOT: u16 = 3312;

pub const MAX_SNAPSHOT_CHUNK: usize = 400;
pub const MAX_FUTURE_SKEW_MS: u64 = 60 * 60 * 1000;

const TAG_INVITE: &str = "invite";
const TAG_TARGET: &str = "p";
const TAG_SNAP: &str = "snap";
const TAG_CONTENT: &str = "content";
const CONTENT_JOIN: &str = "join";
const CONTENT_LEAVE: &str = "leave";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestbookEntry {
    Join {
        member: PublicKey,
        at_ms: u64,
        /// The `(creator, label)` an invite attributed the join to.
        invited_by: Option<(String, String)>,
    },
    Leave {
        member: PublicKey,
        at_ms: u64,
    },
    Kick {
        actor: PublicKey,
        target: PublicKey,
        at_ms: u64,
        citation: Option<AuthorityCitation>,
    },
    Snapshot {
        refounder: PublicKey,
        members: Vec<PublicKey>,
        snapshot_id: [u8; 32],
        chunk: (u32, u32),
        at_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestbookRumor {
    pub id: EventId,
    pub author: PublicKey,
    pub kind: Kind,
    pub at_ms: u64,
    pub entry: GuestbookEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberState {
    Joined {
        at_ms: u64,
        invited_by: Option<(String, String)>,
    },
    Left {
        at_ms: u64,
    },
    Kicked {
        at_ms: u64,
        actor: PublicKey,
    },
}

pub fn build_join(
    member: PublicKey,
    invited_by: Option<(&str, &str)>,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = Vec::new();

    if let Some((creator, label)) = invited_by {
        tags.push(Tag::custom(TAG_INVITE, [creator, label]));
    }

    build_rumor_ms(KIND_JOIN_LEAVE, member, CONTENT_JOIN, tags, at_ms)
}

pub fn build_leave(member: PublicKey, at_ms: u64) -> UnsignedEvent {
    build_rumor_ms(KIND_JOIN_LEAVE, member, CONTENT_LEAVE, Vec::new(), at_ms)
}

pub fn build_kick(
    actor: PublicKey,
    target: &PublicKey,
    citation: Option<&AuthorityCitation>,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = vec![Tag::custom(TAG_TARGET, [target.to_hex()])];

    if let Some(citation) = citation {
        tags.push(citation_tag(citation));
    }

    build_rumor_ms(KIND_KICK, actor, "", tags, at_ms)
}

pub fn build_snapshot_chunks(
    refounder: PublicKey,
    members: &[PublicKey],
    snapshot_id: [u8; 32],
    at_ms: u64,
) -> Vec<UnsignedEvent> {
    let chunks: Vec<&[PublicKey]> = members.chunks(MAX_SNAPSHOT_CHUNK).collect();
    let total = chunks.len() as u32;

    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let hex: Vec<String> = chunk.iter().map(PublicKey::to_hex).collect();
            let content = format!(
                "[{}]",
                hex.iter()
                    .map(|member| format!("\"{member}\""))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let tags = vec![Tag::custom(
                TAG_SNAP,
                [
                    HEXLOWER.encode(&snapshot_id),
                    (index as u32 + 1).to_string(),
                    total.to_string(),
                ],
            )];

            build_rumor_ms(KIND_SNAPSHOT, refounder, &content, tags, at_ms)
        })
        .collect()
}

pub async fn seal_rumor<S>(
    rumor: &UnsignedEvent,
    group: &GroupKey,
    author: &S,
) -> Result<(Event, Keys), GuestbookError>
where
    S: AsyncGetPublicKey + AsyncSignEvent + ?Sized,
{
    let kind = rumor.kind.as_u16();

    if !is_guestbook_kind(kind) {
        return Err(GuestbookError::UnknownKind(kind));
    }

    let seal = build_seal(rumor, SealForm::Encrypted, group, author).await?;

    Ok(wrap_seal(&seal, group, KIND_WRAP, rumor.created_at, &[])?)
}

pub fn open(
    wrap: &Event,
    group: &GroupKey,
) -> Result<(OpenedStream, GuestbookRumor), GuestbookError> {
    let opened = open_wrap(wrap, group)?;

    if opened.seal_form != SealForm::Encrypted {
        return Err(GuestbookError::NotEncryptedSealed);
    }

    let entry = entry_of(&opened)?;
    let rumor = GuestbookRumor {
        id: opened.rumor_id,
        author: opened.author,
        kind: opened.rumor.kind,
        at_ms: opened.at_ms,
        entry,
    };

    Ok((opened, rumor))
}

/// Coalesce the guestbook flat: one final state per npub, the latest entry
/// winning by millisecond time, ties broken by the lower rumor id.
///
/// `snapshot_authorities` are the npubs whose refounding is known to have minted an epoch this client reads.
/// A snapshot chunk is honored only from one of them, and an empty set honors no snapshot at all.
pub fn coalesce(
    rumors: &[GuestbookRumor],
    now_ms: u64,
    snapshot_authorities: &BTreeSet<PublicKey>,
    can_kick: impl Fn(&PublicKey, &PublicKey, Option<&AuthorityCitation>) -> bool,
) -> BTreeMap<PublicKey, MemberState> {
    let mut states: BTreeMap<PublicKey, (u64, Reverse<EventId>, MemberState)> = BTreeMap::new();
    let horizon = now_ms.saturating_add(MAX_FUTURE_SKEW_MS);

    for rumor in rumors {
        if rumor.at_ms > horizon {
            continue;
        }

        match &rumor.entry {
            GuestbookEntry::Join {
                member,
                at_ms,
                invited_by,
            } => offer(
                &mut states,
                *member,
                *at_ms,
                rumor.id,
                MemberState::Joined {
                    at_ms: *at_ms,
                    invited_by: invited_by.clone(),
                },
            ),
            GuestbookEntry::Leave { member, at_ms } => offer(
                &mut states,
                *member,
                *at_ms,
                rumor.id,
                MemberState::Left { at_ms: *at_ms },
            ),
            GuestbookEntry::Kick {
                actor,
                target,
                at_ms,
                citation,
            } => {
                if !can_kick(actor, target, citation.as_ref()) {
                    continue;
                }

                offer(
                    &mut states,
                    *target,
                    *at_ms,
                    rumor.id,
                    MemberState::Kicked {
                        at_ms: *at_ms,
                        actor: *actor,
                    },
                );
            }
            GuestbookEntry::Snapshot {
                refounder,
                members,
                at_ms,
                ..
            } => {
                if !snapshot_authorities.contains(refounder) {
                    continue;
                }

                for member in members {
                    offer(
                        &mut states,
                        *member,
                        *at_ms,
                        rumor.id,
                        MemberState::Joined {
                            at_ms: *at_ms,
                            invited_by: None,
                        },
                    );
                }
            }
        }
    }

    states
        .into_iter()
        .map(|(member, (_, _, state))| (member, state))
        .collect()
}

pub fn complete_memberlist(
    coalesced: &BTreeMap<PublicKey, MemberState>,
    observed: &BTreeMap<PublicKey, u64>,
    granted: &BTreeSet<PublicKey>,
    banned: &BTreeSet<PublicKey>,
    banned_at: &BTreeMap<PublicKey, u64>,
) -> BTreeSet<PublicKey> {
    let mut candidates: BTreeSet<&PublicKey> = coalesced.keys().collect();
    candidates.extend(observed.keys());
    candidates.extend(granted.iter());

    let mut members = BTreeSet::new();

    for member in candidates {
        let mut inclusion = observed.get(member).copied();

        if let Some(state) = coalesced.get(member) {
            match state {
                MemberState::Joined { at_ms, .. } => {
                    inclusion = Some(inclusion.map_or(*at_ms, |seen| seen.max(*at_ms)));
                }
                MemberState::Left { .. } | MemberState::Kicked { .. } => {}
            }
        }

        if inclusion.is_none() && granted.contains(member) {
            inclusion = Some(0);
        }

        let mut exclusion = match coalesced.get(member) {
            Some(MemberState::Left { at_ms }) | Some(MemberState::Kicked { at_ms, .. }) => {
                Some(*at_ms)
            }
            _ => None,
        };

        if banned.contains(member) {
            exclusion = Some(match banned_at.get(member) {
                Some(at_ms) => exclusion.map_or(*at_ms, |seen| seen.max(*at_ms)),
                None => u64::MAX,
            });
        }

        if let Some(inclusion) = inclusion
            && exclusion.is_none_or(|exclusion| inclusion > exclusion)
        {
            members.insert(*member);
        }
    }

    members
}

fn offer(
    states: &mut BTreeMap<PublicKey, (u64, Reverse<EventId>, MemberState)>,
    member: PublicKey,
    at_ms: u64,
    id: EventId,
    state: MemberState,
) {
    let candidate = (at_ms, Reverse(id));

    if let Some(existing) = states.get(&member)
        && (existing.0, existing.1) >= candidate
    {
        return;
    }

    states.insert(member, (at_ms, Reverse(id), state));
}

fn is_guestbook_kind(kind: u16) -> bool {
    matches!(kind, KIND_JOIN_LEAVE | KIND_KICK | KIND_SNAPSHOT)
}

fn entry_of(opened: &OpenedStream) -> Result<GuestbookEntry, GuestbookError> {
    let rumor = &opened.rumor;
    let author = opened.author;
    let at_ms = opened.at_ms;

    match rumor.kind.as_u16() {
        KIND_JOIN_LEAVE => match rumor.content.as_str() {
            CONTENT_JOIN => Ok(GuestbookEntry::Join {
                member: author,
                at_ms,
                invited_by: invite_of(rumor),
            }),
            CONTENT_LEAVE => Ok(GuestbookEntry::Leave {
                member: author,
                at_ms,
            }),
            _ => Err(GuestbookError::BadTag(TAG_CONTENT)),
        },
        KIND_KICK => Ok(GuestbookEntry::Kick {
            actor: author,
            target: tagged_pubkey(rumor, TAG_TARGET)?,
            at_ms,
            citation: optional_citation(rumor)?,
        }),
        KIND_SNAPSHOT => {
            let (snapshot_id, chunk) = snapshot_of(rumor)?;
            let members = members_of(&rumor.content)?;

            Ok(GuestbookEntry::Snapshot {
                refounder: author,
                members,
                snapshot_id,
                chunk,
                at_ms,
            })
        }
        other => Err(GuestbookError::UnknownKind(other)),
    }
}

fn invite_of(rumor: &UnsignedEvent) -> Option<(String, String)> {
    rumor.tags.iter().find_map(|candidate| {
        let fields = candidate.as_slice();

        (fields.len() >= 3 && fields[0] == TAG_INVITE)
            .then(|| (fields[1].clone(), fields[2].clone()))
    })
}

fn members_of(content: &str) -> Result<Vec<PublicKey>, GuestbookError> {
    let entries: Vec<String> =
        serde_json::from_str(content).map_err(|_| GuestbookError::BadTag(TAG_CONTENT))?;

    if entries.len() > MAX_SNAPSHOT_CHUNK {
        return Err(GuestbookError::BadTag(TAG_SNAP));
    }

    entries
        .iter()
        .map(|entry| pubkey(entry, TAG_CONTENT))
        .collect()
}

fn snapshot_of(rumor: &UnsignedEvent) -> Result<([u8; 32], (u32, u32)), GuestbookError> {
    let fields = required(rumor, TAG_SNAP)?;

    if fields.len() != 4 {
        return Err(GuestbookError::BadTag(TAG_SNAP));
    }

    let snapshot_id = decode_hex_32(&fields[1]).map_err(|_| GuestbookError::BadTag(TAG_SNAP))?;
    let index = decimal(&fields[2])?;
    let total = decimal(&fields[3])?;

    if index == 0 || index > total {
        return Err(GuestbookError::BadTag(TAG_SNAP));
    }

    Ok((snapshot_id, (index, total)))
}

fn decimal(raw: &str) -> Result<u32, GuestbookError> {
    canonical_decimal(raw)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(GuestbookError::BadTag(TAG_SNAP))
}

fn tagged_pubkey(rumor: &UnsignedEvent, name: &'static str) -> Result<PublicKey, GuestbookError> {
    pubkey(value(required(rumor, name)?, name)?, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cord01::{StreamError, build_rumor_secs};
    use crate::cord04::TAG_CITATION;
    use crate::derive::guestbook_group_key;
    use crate::{CommunityId, Epoch};

    const ROOT: [u8; 32] = [0x5au8; 32];
    const AT: u64 = 1_700_000_000_000;

    fn community() -> CommunityId {
        CommunityId::from_bytes([0x11u8; 32])
    }

    /// The refounders a fold is told about: a snapshot seeds members on theirs alone.
    fn refounders(keys: &[&Keys]) -> BTreeSet<PublicKey> {
        keys.iter().map(|keys| keys.public_key()).collect()
    }

    fn group() -> GroupKey {
        guestbook_group_key(&ROOT, &community(), Epoch(0)).expect("derives")
    }

    fn citation() -> AuthorityCitation {
        AuthorityCitation {
            entity: [0x33u8; 32],
            version: 1,
            hash: [0x44u8; 32],
        }
    }

    fn publish(rumor: &UnsignedEvent, author: &Keys) -> GuestbookRumor {
        let wrap = smol::block_on(seal_rumor(rumor, &group(), author))
            .expect("seals")
            .0;

        open(&wrap, &group()).expect("opens").1
    }

    #[test]
    fn join_leave_kick_and_snapshot_converge_to_one_memberlist() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let carol = Keys::generate();
        let dave = Keys::generate();
        let frank = Keys::generate();
        let grace = Keys::generate();
        let owner = Keys::generate();

        let survivors: Vec<PublicKey> = (0..401).map(|_| Keys::generate().public_key()).collect();

        let mut rumors = vec![
            publish(
                &build_join(
                    alice.public_key(),
                    Some((&"ab".repeat(32), "Reddit")),
                    AT + 1_000,
                ),
                &alice,
            ),
            publish(&build_join(bob.public_key(), None, AT + 2_000), &bob),
            publish(&build_leave(bob.public_key(), AT + 3_000), &bob),
            publish(&build_join(dave.public_key(), None, AT + 4_000), &dave),
            publish(
                &build_kick(
                    carol.public_key(),
                    &dave.public_key(),
                    Some(&citation()),
                    AT + 5_000,
                ),
                &carol,
            ),
            publish(&build_join(frank.public_key(), None, AT + 7_000), &frank),
        ];

        let snapshot_id = "77".repeat(32);
        let chunks =
            build_snapshot_chunks(carol.public_key(), &survivors, [0x77u8; 32], AT + 6_000);
        assert_eq!(chunks.len(), 2, "401 survivors chunk into two events");
        for (index, chunk) in chunks.iter().enumerate() {
            assert!(chunk.tags.iter().any(|tag| tag.as_slice()
                == [
                    TAG_SNAP,
                    snapshot_id.as_str(),
                    &(index + 1).to_string(),
                    "2"
                ]));
            rumors.push(publish(chunk, &carol));
        }

        let can_kick =
            |actor: &PublicKey, target: &PublicKey, citation: Option<&AuthorityCitation>| {
                citation.is_some() && actor == &carol.public_key() && target != &owner.public_key()
            };

        let states = coalesce(&rumors, AT + 8_000, &refounders(&[&carol]), can_kick);

        assert_eq!(
            states.get(&alice.public_key()),
            Some(&MemberState::Joined {
                at_ms: AT + 1_000,
                invited_by: Some(("ab".repeat(32), "Reddit".to_owned())),
            })
        );
        assert_eq!(
            states.get(&bob.public_key()),
            Some(&MemberState::Left { at_ms: AT + 3_000 })
        );
        assert_eq!(
            states.get(&dave.public_key()),
            Some(&MemberState::Kicked {
                at_ms: AT + 5_000,
                actor: carol.public_key(),
            })
        );
        assert!(
            survivors
                .iter()
                .all(|member| matches!(states.get(member), Some(MemberState::Joined { .. }))),
            "every chunk seeds its own members"
        );

        let reversed: Vec<GuestbookRumor> = rumors.iter().rev().cloned().collect();
        assert_eq!(
            coalesce(&reversed, AT + 8_000, &refounders(&[&carol]), can_kick),
            states,
            "arrival order cannot change the fold"
        );

        let observed = BTreeMap::from([
            (bob.public_key(), AT + 9_000),
            (carol.public_key(), AT + 5_000),
        ]);
        let granted = BTreeSet::from([grace.public_key()]);
        let banned = BTreeSet::from([frank.public_key()]);
        let banned_at = BTreeMap::from([(frank.public_key(), AT + 8_000)]);

        let members = complete_memberlist(&states, &observed, &granted, &banned, &banned_at);

        let mut expected = BTreeSet::from([
            alice.public_key(),
            bob.public_key(),
            carol.public_key(),
            grace.public_key(),
        ]);
        expected.extend(survivors.iter().copied());

        assert_eq!(members, expected);
        assert!(
            !members.contains(&dave.public_key()),
            "a kicked member is out"
        );
        assert!(
            !members.contains(&frank.public_key()),
            "a ban wins over a later join"
        );
    }

    #[test]
    fn a_kick_or_snapshot_without_authority_is_dropped() {
        let moderator = Keys::generate();
        let outsider = Keys::generate();
        let owner = Keys::generate();
        let kicked = Keys::generate();
        let uncited = Keys::generate();
        let unranked = Keys::generate();
        let refounder = Keys::generate();
        let impostor = Keys::generate();
        let seeded = Keys::generate();
        let smuggled = Keys::generate();

        let can_kick = |actor: &PublicKey,
                        target: &PublicKey,
                        citation: Option<&AuthorityCitation>| {
            citation.is_some() && actor == &moderator.public_key() && target != &owner.public_key()
        };

        let rumors = vec![
            publish(
                &build_kick(
                    moderator.public_key(),
                    &kicked.public_key(),
                    Some(&citation()),
                    AT,
                ),
                &moderator,
            ),
            publish(
                &build_kick(moderator.public_key(), &uncited.public_key(), None, AT),
                &moderator,
            ),
            publish(
                &build_kick(
                    outsider.public_key(),
                    &unranked.public_key(),
                    Some(&citation()),
                    AT,
                ),
                &outsider,
            ),
            publish(
                &build_kick(
                    moderator.public_key(),
                    &owner.public_key(),
                    Some(&citation()),
                    AT,
                ),
                &moderator,
            ),
        ];

        let states = coalesce(&rumors, AT + 1_000, &BTreeSet::new(), can_kick);

        assert_eq!(
            states.get(&kicked.public_key()),
            Some(&MemberState::Kicked {
                at_ms: AT,
                actor: moderator.public_key(),
            })
        );
        assert!(
            !states.contains_key(&uncited.public_key()),
            "a kick cites the Grant it acts under"
        );
        assert!(
            !states.contains_key(&unranked.public_key()),
            "a kick needs KICK"
        );
        assert!(
            !states.contains_key(&owner.public_key()),
            "nobody kicks the owner"
        );

        let by_refounder = build_snapshot_chunks(
            refounder.public_key(),
            &[seeded.public_key()],
            [0x77u8; 32],
            AT,
        )
        .remove(0);
        let by_impostor = build_snapshot_chunks(
            impostor.public_key(),
            &[smuggled.public_key()],
            [0x88u8; 32],
            AT,
        )
        .remove(0);

        for authority in [BTreeSet::new(), refounders(&[&refounder])] {
            let states = coalesce(
                &[
                    publish(&by_refounder, &refounder),
                    publish(&by_impostor, &impostor),
                ],
                AT + 1_000,
                &authority,
                |_, _, _| true,
            );

            assert_eq!(
                states.contains_key(&seeded.public_key()),
                !authority.is_empty(),
                "only a known refounder seeds, and there is no owner fallback"
            );
            assert!(
                !states.contains_key(&smuggled.public_key()),
                "a foreign snapshot never seeds"
            );
        }
    }

    #[test]
    fn a_future_entry_a_bad_ms_and_a_malformed_snapshot_are_dropped() {
        let member = Keys::generate();
        let moderator = Keys::generate();
        let target = Keys::generate();

        let future = publish(
            &build_join(member.public_key(), None, AT + MAX_FUTURE_SKEW_MS + 1),
            &member,
        );
        let horizon = publish(
            &build_join(member.public_key(), None, AT + MAX_FUTURE_SKEW_MS),
            &member,
        );
        assert!(
            coalesce(&[future], AT, &BTreeSet::new(), |_, _, _| true).is_empty(),
            "an entry more than an hour ahead is dropped"
        );
        assert_eq!(
            coalesce(&[horizon], AT, &BTreeSet::new(), |_, _, _| true).len(),
            1,
            "the horizon itself is skew, not forgery"
        );

        let bad_ms = build_rumor_secs(
            KIND_JOIN_LEAVE,
            member.public_key(),
            CONTENT_JOIN,
            vec![Tag::custom("ms", ["1000"])],
            AT / 1000,
        );
        assert!(matches!(
            open(
                &smol::block_on(seal_rumor(&bad_ms, &group(), &member))
                    .expect("seals")
                    .0,
                &group()
            ),
            Err(GuestbookError::Stream(StreamError::BadMs))
        ));

        let bad_verb = build_rumor_ms(KIND_JOIN_LEAVE, member.public_key(), "maybe", vec![], AT);
        assert!(matches!(
            open(
                &smol::block_on(seal_rumor(&bad_verb, &group(), &member))
                    .expect("seals")
                    .0,
                &group()
            ),
            Err(GuestbookError::BadTag(TAG_CONTENT))
        ));

        let ambiguous = build_rumor_ms(
            KIND_KICK,
            moderator.public_key(),
            "",
            vec![
                Tag::custom(TAG_TARGET, [target.public_key().to_hex()]),
                citation_tag(&citation()),
                citation_tag(&citation()),
            ],
            AT,
        );
        assert!(matches!(
            open(
                &smol::block_on(seal_rumor(&ambiguous, &group(), &moderator))
                    .expect("seals")
                    .0,
                &group()
            ),
            Err(GuestbookError::DuplicateTag(TAG_CITATION))
        ));

        for fields in [
            vec![snapshot_id(), "0".to_owned(), "2".to_owned()],
            vec![snapshot_id(), "3".to_owned(), "2".to_owned()],
            vec![snapshot_id(), "1".to_owned()],
        ] {
            let rumor = build_rumor_ms(
                KIND_SNAPSHOT,
                moderator.public_key(),
                "[]",
                vec![Tag::custom(TAG_SNAP, fields)],
                AT,
            );
            assert!(matches!(
                open(
                    &smol::block_on(seal_rumor(&rumor, &group(), &moderator))
                        .expect("seals")
                        .0,
                    &group()
                ),
                Err(GuestbookError::BadTag(TAG_SNAP))
            ));
        }
    }

    fn snapshot_id() -> String {
        "ab".repeat(32)
    }
}
