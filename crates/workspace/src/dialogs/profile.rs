use std::str::FromStr;

use anyhow::Error;
use chat::{ChatRegistry, Room, RoomKind};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClipboardItem, Context, Div, Entity, IntoElement, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Task, Window, div, px, retain_all,
};
use instant::Duration;
use nostr_sdk::prelude::*;
use person::{Person, PersonRegistry, shorten_pubkey};
use settings::AppSettings;
use state::{NostrRegistry, upload};
use theme::{ActiveTheme, TextStyle, Typeset as _, button_radius};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants, IconPosition};
use ui::dock::DockPlacement;
use ui::input::{Input, InputState, Textarea, TextareaState};
use ui::notification::Notification;
use ui::{Disableable, IconName, Sizable, StyledExt, WindowExtension, h_flex, v_flex};

use crate::Workspace;

pub fn open(public_key: PublicKey, window: &mut Window, cx: &mut App) {
    let profile = cx.new(|cx| ProfileDialog::new(public_key, window, cx));

    window.open_dialog(cx, move |this, _window, _cx| {
        this.title("Profile").child(profile.clone())
    });
}

struct ProfileDialog {
    public_key: PublicKey,
    /// Whether the profile belongs to the signed-in user
    is_current_user: bool,
    /// Whether the metadata inputs are shown
    editing: bool,
    /// User's name text input
    name_input: Entity<InputState>,
    /// User's avatar url text input
    avatar_input: Entity<InputState>,
    /// User's bio multi line input
    bio_input: Entity<TextareaState>,
    /// User's website url text input
    website_input: Entity<InputState>,
    /// Uploading state
    uploading: bool,
    /// Copied states
    copied: bool,
    /// Updating state
    updating: bool,
    /// Tasks
    tasks: Vec<Task<Result<(), Error>>>,
}

impl ProfileDialog {
    fn new(public_key: PublicKey, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let nostr = NostrRegistry::global(cx);
        let is_current_user = nostr
            .read(cx)
            .current_user()
            .is_some_and(|me| me == public_key);

        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Alice"));
        let website_input = cx.new(|cx| InputState::new(window, cx).placeholder("alice.me"));

        let avatar_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("alice.me/avatar.jpg"));

        let bio_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("A short introduce about you.")
        });

        Self {
            public_key,
            is_current_user,
            editing: false,
            name_input,
            avatar_input,
            bio_input,
            website_input,
            uploading: false,
            copied: false,
            updating: false,
            tasks: vec![],
        }
    }

    fn profile(&self, cx: &App) -> Person {
        let persons = PersonRegistry::global(cx);
        persons.read(cx).get(&self.public_key, cx)
    }

    fn set_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let metadata = self.profile(cx).metadata();

        self.avatar_input.update(cx, |this, cx| {
            if let Some(avatar) = metadata.picture.as_ref() {
                this.set_value(avatar, window, cx);
            }
        });

        self.bio_input.update(cx, |this, cx| {
            if let Some(bio) = metadata.about.as_ref() {
                this.set_value(bio, window, cx);
            }
        });

        self.name_input.update(cx, |this, cx| {
            if let Some(display_name) = metadata.display_name.as_ref() {
                this.set_value(display_name, window, cx);
            }
        });

        self.website_input.update(cx, |this, cx| {
            if let Some(website) = metadata.website.as_ref() {
                this.set_value(website, window, cx);
            }
        });
    }

    /// Toggle the metadata inputs, filling them with the saved profile when shown
    fn toggle_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = !self.editing;

        if self.editing {
            self.set_profile(window, cx);
        }

        cx.notify();
    }

    fn copy(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let item = ClipboardItem::new_string(value);
        cx.write_to_clipboard(item);

        self.set_copied(true, window, cx);
    }

    fn set_copied(&mut self, status: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.copied = status;
        cx.notify();

        if status {
            self.tasks.push(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(2)).await;

                // Reset the copied state after a delay
                cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        this.set_copied(false, window, cx);
                    })
                    .ok();
                })
                .ok();

                Ok(())
            }));
        }
    }

    fn set_uploading(&mut self, status: bool, cx: &mut Context<Self>) {
        self.uploading = status;
        cx.notify();
    }

    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Get the user's configured blossom server
        let server = AppSettings::get_file_server(cx);

        // Ask user for file upload
        let path = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            // Selecting no file means the prompt was cancelled
            let Some(path) = path.await??.and_then(|mut paths| paths.pop()) else {
                return Ok(());
            };

            this.update(cx, |this, cx| {
                this.set_uploading(true, cx);
            })?;

            // Upload via blossom client
            match upload(server, path, cx).await {
                Ok(url) => {
                    this.update_in(cx, |this, window, cx| {
                        this.avatar_input.update(cx, |this, cx| {
                            this.set_value(url.to_string(), window, cx);
                        });
                        this.set_uploading(false, cx);
                    })?;
                }
                Err(e) => {
                    this.update_in(cx, |this, window, cx| {
                        this.set_uploading(false, cx);
                        window.push_notification(
                            Notification::error(e.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            }

            Ok(())
        }));
    }

    fn set_updating(&mut self, updating: bool, cx: &mut Context<Self>) {
        self.updating = updating;
        cx.notify();
    }

    /// Set the metadata for the current user
    fn publish(&self, metadata: &Metadata, cx: &App) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();
        let metadata = metadata.clone();

        cx.background_spawn(async move {
            // Build and sign the metadata event
            let event = metadata.finalize_async(&signer).await?;

            // Send event to user's relays
            client.send_event(&event).await?;

            Ok(())
        })
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let persons = PersonRegistry::global(cx);
        let public_key = self.public_key;
        let old_metadata = persons.read(cx).get(&public_key, cx).metadata();

        // Extract all new metadata fields
        let avatar = self.avatar_input.read(cx).value();
        let name = self.name_input.read(cx).value();
        let bio = self.bio_input.read(cx).value();
        let website = self.website_input.read(cx).value();

        // Construct the new metadata
        let mut new_metadata = old_metadata
            .display_name(name.as_ref())
            .name(name.as_ref())
            .about(bio.as_ref());

        // Verify the avatar URL before adding it
        if let Ok(url) = Url::from_str(&avatar) {
            new_metadata = new_metadata.picture(url);
        }

        // Verify the website URL before adding it
        if let Ok(url) = Url::from_str(&website) {
            new_metadata = new_metadata.website(url);
        }

        // Set the metadata
        let task = self.publish(&new_metadata, cx);

        // Set the updating state
        self.set_updating(true, cx);

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update_in(cx, |this, window, cx| {
                        // Update the registry
                        persons.update(cx, |this, cx| {
                            this.insert(Person::new(public_key, new_metadata), cx);
                        });

                        this.set_updating(false, cx);
                        this.editing = false;
                        this.set_profile(window, cx);

                        window.push_notification("Profile updated successfully", cx);
                    })?;
                }
                Err(e) => {
                    cx.update(|window, cx| {
                        window.push_notification(
                            Notification::error(e.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            };

            Ok(())
        }));
    }

    /// Open the conversation with this person, creating an ongoing one if needed
    fn message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let chat = ChatRegistry::global(cx);

        let Some(current_user) = nostr.read(cx).current_user() else {
            return;
        };

        let existing = chat
            .read(cx)
            .rooms(&RoomKind::Ongoing, cx)
            .into_iter()
            .chain(chat.read(cx).rooms(&RoomKind::Request, cx))
            .find(|room| {
                let room = room.read(cx);
                !room.is_group() && room.members().contains(&self.public_key)
            });

        let room = match existing {
            Some(room) => room,
            None => chat.update(cx, |chat, cx| {
                let room = cx.new(|_| {
                    Room::new(current_user, [self.public_key])
                        .organize(&current_user)
                        .kind(RoomKind::Ongoing)
                });
                chat.track_room(&room, cx);
                room
            }),
        };

        Workspace::add_panel(
            chat_ui::init(room.downgrade(), window, cx),
            DockPlacement::Center,
            window,
            cx,
        );

        window.close_dialog(cx);
    }
}

impl Render for ProfileDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let profile = self.profile(cx);
        let shorten_pkey = SharedString::from(shorten_pubkey(self.public_key, 8));
        let avatar_url = self.avatar_input.read(cx).value();

        let avatar = if self.editing && !avatar_url.is_empty() {
            Some(avatar_url)
        } else {
            profile.avatar()
        };

        v_flex()
            .image_cache(retain_all("profile-dialog"))
            .gap_3()
            .w_full()
            .child(
                v_flex()
                    .gap_4()
                    .h_40()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .bg(cx.theme().surface)
                    .child(Avatar::new(avatar).seed(profile.avatar_seed()).large())
                    .when(self.editing, |this| {
                        this.child(
                            Button::new("upload")
                                .icon(IconName::ArrowRight)
                                .icon_position(IconPosition::End)
                                .label("Add an avatar")
                                .xsmall()
                                .ghost()
                                .rounded()
                                .disabled(self.uploading)
                                .loading(self.uploading)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.upload(window, cx);
                                })),
                        )
                    }),
            )
            .child(
                v_flex().gap_1p5().child(label("Public Key", cx)).child(
                    h_flex()
                        .h_8()
                        .w_full()
                        .justify_center()
                        .gap_3()
                        .rounded(px(button_radius()))
                        .bg(cx.theme().surface)
                        .text_style(TextStyle::Callout)
                        .child(shorten_pkey)
                        .child(
                            Button::new("copy")
                                .icon({
                                    if self.copied {
                                        IconName::CheckCircle
                                    } else {
                                        IconName::Copy
                                    }
                                })
                                .xsmall()
                                .secondary()
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    let pk = this.public_key.to_bech32().unwrap();
                                    this.copy(pk, window, cx)
                                })),
                        ),
                ),
            )
            .when(self.editing, |this| {
                this.child(
                    v_flex()
                        .gap_1p5()
                        .child(label("Name", cx))
                        .child(Input::new(&self.name_input).small()),
                )
                .child(
                    v_flex()
                        .gap_1p5()
                        .child(label("About", cx))
                        .child(Textarea::new(&self.bio_input).small()),
                )
                .child(
                    v_flex()
                        .gap_1p5()
                        .child(label("Website", cx))
                        .child(Input::new(&self.website_input).small()),
                )
            })
            .when(!self.editing, |this| {
                let metadata = profile.metadata();

                let name = metadata
                    .display_name
                    .filter(|name| !name.trim().is_empty())
                    .or(metadata.name.filter(|name| !name.trim().is_empty()))
                    .map(SharedString::from);

                let about = metadata
                    .about
                    .filter(|about| !about.trim().is_empty())
                    .map(SharedString::from);

                let website = metadata
                    .website
                    .filter(|website| !website.trim().is_empty())
                    .map(SharedString::from);

                this.child(field("Name", name, "Unknown", cx))
                    .child(field("About", about, "No bio", cx))
                    .child(field("Website", website, "No website", cx))
            })
            .child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .when(self.is_current_user && !self.editing, |this| {
                        this.child(
                            Button::new("update")
                                .icon(IconName::Edit)
                                .icon_position(IconPosition::End)
                                .label("Update")
                                .primary()
                                .font_semibold()
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.toggle_edit(window, cx);
                                })),
                        )
                    })
                    .when(!self.is_current_user, |this| {
                        this.child(
                            Button::new("message")
                                .icon(IconName::Message)
                                .icon_position(IconPosition::End)
                                .label("Message")
                                .primary()
                                .font_semibold()
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.message(window, cx);
                                })),
                        )
                    })
                    .when(self.editing, |this| {
                        this.child(
                            Button::new("cancel")
                                .label("Cancel")
                                .ghost()
                                .disabled(self.updating)
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.toggle_edit(window, cx);
                                })),
                        )
                        .child(
                            Button::new("save")
                                .icon(IconName::CheckCircle)
                                .icon_position(IconPosition::End)
                                .label("Save")
                                .primary()
                                .font_semibold()
                                .loading(self.updating)
                                .disabled(self.updating)
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.save(window, cx);
                                })),
                        )
                    }),
            )
    }
}

fn label(label: &'static str, cx: &App) -> Div {
    div()
        .text_style(TextStyle::Caption)
        .text_color(cx.theme().text_muted)
        .font_semibold()
        .child(label)
}

fn field(
    text: &'static str,
    value: Option<SharedString>,
    placeholder: &'static str,
    cx: &App,
) -> Div {
    let (content, color) = match value {
        Some(value) => (value, cx.theme().text),
        None => (SharedString::from(placeholder), cx.theme().text_muted),
    };

    v_flex().gap_1p5().child(label(text, cx)).child(
        div()
            .text_style(TextStyle::Callout)
            .text_color(color)
            .child(content),
    )
}
