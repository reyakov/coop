use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Error};
use common::{shorten_pubkey, RenderedTimestamp};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, relative, rems, uniform_list, App, AppContext, Context, Div, Entity,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window,
};
use nostr_sdk::prelude::*;
use person::{Person, PersonRegistry};
use smallvec::{smallvec, SmallVec};
use state::{NostrAddress, NostrRegistry, BOOTSTRAP_RELAYS, TIMEOUT};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::indicator::Indicator;
use ui::{h_flex, v_flex, Icon, IconName, Sizable, StyledExt, WindowExtension};

pub fn init(public_key: PublicKey, window: &mut Window, cx: &mut App) -> Entity<Screening> {
    cx.new(|cx| Screening::new(public_key, window, cx))
}

/// Screening
pub struct Screening {
    /// Public Key of the person being screened.
    public_key: PublicKey,

    /// Whether the person's address is verified.
    verified: bool,

    /// Whether the person is followed by current user.
    followed: bool,

    /// Last time the person was active.
    last_active: Option<Timestamp>,

    /// All mutual contacts of the person being screened.
    mutual_contacts: Vec<PublicKey>,

    /// Async tasks
    tasks: SmallVec<[Task<()>; 3]>,
}

impl Screening {
    pub fn new(public_key: PublicKey, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.defer_in(window, move |this, _window, cx| {
            this.check_contact(cx);
            this.check_wot(cx);
            this.check_last_activity(cx);
            this.verify_identifier(cx);
        });

        Self {
            public_key,
            verified: false,
            followed: false,
            last_active: None,
            mutual_contacts: vec![],
            tasks: smallvec![],
        }
    }

    fn check_contact(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let public_key = self.public_key;

        let task: Task<Result<bool, Error>> = cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let signer_pubkey = signer.get_public_key().await?;

            // Check if user is in contact list
            let contacts = client.database().contacts_public_keys(signer_pubkey).await;
            let followed = contacts.unwrap_or_default().contains(&public_key);

            Ok(followed)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or(false);

            this.update(cx, |this, cx| {
                this.followed = result;
                cx.notify();
            })
            .ok();
        }));
    }

    fn check_wot(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let public_key = self.public_key;

        let task: Task<Result<Vec<PublicKey>, Error>> = cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let signer_pubkey = signer.get_public_key().await?;

            // Check mutual contacts
            let filter = Filter::new().kind(Kind::ContactList).pubkey(public_key);
            let mut mutual_contacts = vec![];

            if let Ok(events) = client.database().query(filter).await {
                for event in events.into_iter().filter(|ev| ev.pubkey != signer_pubkey) {
                    mutual_contacts.push(event.pubkey);
                }
            }

            Ok(mutual_contacts)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(contacts) => {
                    this.update(cx, |this, cx| {
                        this.mutual_contacts = contacts;
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    log::error!("Failed to fetch mutual contacts: {}", e);
                }
            };
        }));
    }

    fn check_last_activity(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let public_key = self.public_key;

        let task: Task<Option<Timestamp>> = cx.background_spawn(async move {
            let filter = Filter::new().author(public_key).limit(1);
            let mut activity: Option<Timestamp> = None;

            // Construct target for subscription
            let target = BOOTSTRAP_RELAYS
                .into_iter()
                .map(|relay| (relay, vec![filter.clone()]))
                .collect::<HashMap<_, _>>();

            if let Ok(mut stream) = client
                .stream_events(target)
                .timeout(Duration::from_secs(TIMEOUT))
                .await
            {
                while let Some((_url, event)) = stream.next().await {
                    if let Ok(event) = event {
                        activity = Some(event.created_at);
                    }
                }
            }

            activity
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;

            this.update(cx, |this, cx| {
                this.last_active = result;
                cx.notify();
            })
            .ok();
        }));
    }

    fn verify_identifier(&mut self, cx: &mut Context<Self>) {
        let http_client = cx.http_client();
        let public_key = self.public_key;

        // Skip if the user doesn't have a NIP-05 identifier
        let Some(address) = self.address(cx) else {
            return;
        };

        let task: Task<Result<bool, Error>> =
            cx.background_spawn(async move { address.verify(&http_client, &public_key).await });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or(false);

            this.update(cx, |this, cx| {
                this.verified = result;
                cx.notify();
            })
            .ok();
        }));
    }

    fn profile(&self, cx: &Context<Self>) -> Person {
        let persons = PersonRegistry::global(cx);
        persons.read(cx).get(&self.public_key, cx)
    }

    fn address(&self, cx: &Context<Self>) -> Option<Nip05Address> {
        self.profile(cx)
            .metadata()
            .nip05
            .and_then(|addr| Nip05Address::parse(&addr).ok())
    }

    fn open_njump(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Ok(bech32) = self.profile(cx).public_key().to_bech32();
        cx.open_url(&format!("https://njump.me/{bech32}"));
    }

    fn report(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let public_key = self.public_key;

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let tag = Tag::public_key_report(public_key, Report::Impersonation);
            let builder = EventBuilder::report(vec![tag], "");
            let event = client.sign_event_builder(builder).await?;

            // Send the report to the public relays
            client.send_event(&event).to(BOOTSTRAP_RELAYS).await?;

            Ok(())
        });

        self.tasks.push(cx.spawn_in(window, async move |_, cx| {
            if task.await.is_ok() {
                cx.update(|window, cx| {
                    window.close_modal(cx);
                    window.push_notification("Report submitted successfully", cx);
                })
                .ok();
            }
        }));
    }

    fn mutual_contacts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let contacts = self.mutual_contacts.clone();

        window.open_modal(cx, move |this, _window, _cx| {
            let contacts = contacts.clone();
            let total = contacts.len();

            this.title(SharedString::from("Mutual contacts")).child(
                v_flex().gap_1().pb_4().child(
                    uniform_list("contacts", total, move |range, _window, cx| {
                        let persons = PersonRegistry::global(cx);
                        let mut items = Vec::with_capacity(total);

                        for ix in range {
                            let Some(contact) = contacts.get(ix) else {
                                continue;
                            };
                            let profile = persons.read(cx).get(contact, cx);

                            items.push(
                                h_flex()
                                    .h_11()
                                    .w_full()
                                    .px_2()
                                    .gap_1p5()
                                    .rounded(cx.theme().radius)
                                    .text_sm()
                                    .hover(|this| this.bg(cx.theme().elevated_surface_background))
                                    .child(Avatar::new(profile.avatar()).size(rems(1.75)))
                                    .child(profile.name()),
                            );
                        }

                        items
                    })
                    .h(px(300.)),
                ),
            )
        });
    }
}

impl Render for Screening {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let profile = self.profile(cx);
        let shorten_pubkey = shorten_pubkey(self.public_key, 8);

        let total_mutuals = self.mutual_contacts.len();
        let last_active = self.last_active.map(|_| true);

        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_3()
                    .items_center()
                    .justify_center()
                    .text_center()
                    .child(Avatar::new(profile.avatar()).size(rems(4.)))
                    .child(
                        div()
                            .font_semibold()
                            .line_height(relative(1.25))
                            .child(profile.name()),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .p_1()
                            .flex_1()
                            .h_7()
                            .justify_center()
                            .rounded_full()
                            .bg(cx.theme().surface_background)
                            .text_sm()
                            .truncate()
                            .text_ellipsis()
                            .text_center()
                            .line_height(relative(1.))
                            .child(shorten_pubkey),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("njump")
                                    .label("View on njump.me")
                                    .secondary()
                                    .small()
                                    .rounded()
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        this.open_njump(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("report")
                                    .tooltip("Report as a scam or impostor")
                                    .icon(IconName::Boom)
                                    .danger()
                                    .rounded()
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        this.report(window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .text_sm()
                            .child(status_badge(Some(self.followed), cx))
                            .child(
                                v_flex()
                                    .text_sm()
                                    .child(SharedString::from("Contact"))
                                    .child(
                                        div()
                                            .line_clamp(1)
                                            .text_color(cx.theme().text_muted)
                                            .child({
                                                if self.followed {
                                                    SharedString::from("This person is one of your contacts.")
                                                } else {
                                                    SharedString::from("This person is not one of your contacts.")
                                                }
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .text_sm()
                            .child(status_badge(last_active, cx))
                            .child(
                                v_flex()
                                    .text_sm()
                                    .child(
                                        h_flex()
                                            .gap_0p5()
                                            .child(SharedString::from("Activity on Public Relays"))
                                            .child(
                                                Button::new("active")
                                                    .icon(IconName::Info)
                                                    .xsmall()
                                                    .ghost()
                                                    .rounded()
                                                    .tooltip("This may be inaccurate if the user only publishes to their private relays."),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .line_clamp(1)
                                            .text_color(cx.theme().text_muted)
                                            .map(|this| {
                                                if let Some(date) = self.last_active {
                                                    this.child(SharedString::from(format!(
                                                        "Last active: {}.",
                                                        date.to_human_time()
                                                    )))
                                                } else {
                                                    this.child(SharedString::from("This person hasn't had any activity."))
                                                }
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .child(status_badge(Some(self.verified), cx))
                            .child(
                                v_flex()
                                    .text_sm()
                                    .child({
                                        if let Some(addr) = self.address(cx) {
                                            SharedString::from(format!("{} validation", addr))
                                        } else {
                                            SharedString::from("Friendly Address (NIP-05) validation")
                                        }
                                    })
                                    .child(
                                        div()
                                            .line_clamp(1)
                                            .text_color(cx.theme().text_muted)
                                            .child({
                                                if self.address(cx).is_some() {
                                                    if self.verified {
                                                        SharedString::from("The address matches the user's public key.")
                                                    } else {
                                                        SharedString::from("The address does not match the user's public key.")
                                                    }
                                                } else {
                                                    SharedString::from("This person has not set up their friendly address")
                                                }
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .child(status_badge(Some(total_mutuals > 0), cx))
                            .child(
                                v_flex()
                                    .text_sm()
                                    .child(
                                        h_flex()
                                            .gap_0p5()
                                            .child(SharedString::from("Mutual contacts"))
                                            .child(
                                                Button::new("mutuals")
                                                    .icon(IconName::Info)
                                                    .xsmall()
                                                    .ghost()
                                                    .rounded()
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.mutual_contacts(window, cx);
                                                        },
                                                    )),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .line_clamp(1)
                                            .text_color(cx.theme().text_muted)
                                            .child({
                                                if total_mutuals > 0 {
                                                    SharedString::from(format!(
                                                        "You have {} mutual contacts with this person.",
                                                        total_mutuals
                                                    ))
                                                } else {
                                                    SharedString::from("You don't have any mutual contacts with this person.")
                                                }
                                            }),
                                    ),
                            ),
                    ),
            )
    }
}

fn status_badge(status: Option<bool>, cx: &App) -> Div {
    h_flex()
        .size_6()
        .justify_center()
        .flex_shrink_0()
        .map(|this| {
            if let Some(status) = status {
                this.child(Icon::new(IconName::CheckCircle).small().text_color({
                    if status {
                        cx.theme().icon_accent
                    } else {
                        cx.theme().icon_muted
                    }
                }))
            } else {
                this.child(Indicator::new().small())
            }
        })
}
