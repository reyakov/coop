use gpui::{Action, actions};
use nostr_sdk::prelude::*;
use serde::Deserialize;
use settings::SignerKind;

#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = chat, no_json)]
pub enum Command {
    Insert(String),
    ChangeSubject(String),
    ChangeSigner(SignerKind),
    ToggleBackup,
    Copy(PublicKey),
    Relays(PublicKey),
    Njump(PublicKey),
    Trace(EventId),
}

// Start or stop dictation in the focused composer
actions!(chat, [ToggleDictation]);
