use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Error};
use gpui::{
    div, rems, AnyElement, App, AppContext, ClipboardItem, Context, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, ParentElement, PathPromptOptions, Render, SharedString,
    Styled, Task, Window,
};
use gpui_tokio::Tokio;
use nostr_sdk::prelude::*;
use person::{shorten_pubkey, Person, PersonRegistry};
use settings::AppSettings;
use smol::fs;
use state::{nostr_upload, NostrRegistry};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::input::{InputState, TextInput};
use ui::notification::Notification;
use ui::{h_flex, v_flex, Disableable, IconName, Sizable, StyledExt, WindowExtension};

pub fn init(public_key: PublicKey, window: &mut Window, cx: &mut App) -> Entity<ProfilePanel> {
    cx.new(|cx| ProfilePanel::new(public_key, window, cx))
}

#[derive(Debug)]
pub struct ProfilePanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// User's public key
    public_key: PublicKey,

    /// User's name text input
    name_input: Entity<InputState>,

    /// User's avatar url text input
    avatar_input: Entity<InputState>,

    /// User's bio multi line input
    bio_input: Entity<InputState>,

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

impl ProfilePanel {
    fn new(public_key: PublicKey, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Alice"));
        let website_input = cx.new(|cx| InputState::new(window, cx).placeholder("alice.me"));
        let avatar_input = cx.new(|cx| InputState::new(window, cx).placeholder("alice.me/a.jpg"));

        // Use multi-line input for bio
        let bio_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line()
                .auto_grow(3, 8)
                .placeholder("A short introduce about you.")
        });

        // Get user's profile and update inputs
        cx.defer_in(window, move |this, window, cx| {
            this.set_profile(window, cx);
        });

        Self {
            name: "Update Profile".into(),
            focus_handle: cx.focus_handle(),
            public_key,
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

    fn set_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let persons = PersonRegistry::global(cx);
        let profile = persons.read(cx).get(&self.public_key, cx);
        let metadata = profile.metadata();

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

    fn copy(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let item = ClipboardItem::new_string(value);
        cx.write_to_clipboard(item);

        self.set_copied(true, window, cx);
    }

    fn set_copied(&mut self, status: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.copied = status;
        cx.notify();

        if status {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(2)).await;

                // Reset the copied state after a delay
                cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        this.set_copied(false, window, cx);
                    })
                    .ok();
                })
                .ok();
            })
            .detach();
        }
    }

    fn uploading(&mut self, status: bool, cx: &mut Context<Self>) {
        self.uploading = status;
        cx.notify();
    }

    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.uploading(true, cx);

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        // Get the user's configured NIP96 server
        let nip96_server = AppSettings::get_file_server(cx);

        // Open native file dialog
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        let task = Tokio::spawn(cx, async move {
            match paths.await {
                Ok(Ok(Some(mut paths))) => {
                    if let Some(path) = paths.pop() {
                        let file = fs::read(path).await?;
                        let url = nostr_upload(&client, &nip96_server, file).await?;

                        Ok(url)
                    } else {
                        Err(anyhow!("Path not found"))
                    }
                }
                _ => Err(anyhow!("Error")),
            }
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;

            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(url)) => {
                        this.avatar_input.update(cx, |this, cx| {
                            this.set_value(url.to_string(), window, cx);
                        });
                    }
                    Ok(Err(e)) => {
                        window.push_notification(e.to_string(), cx);
                    }
                    Err(e) => {
                        log::warn!("Failed to upload avatar: {e}");
                    }
                };
                this.uploading(false, cx);
            })
            .expect("Entity has been released");
        })
        .detach();
    }

    fn set_updating(&mut self, updating: bool, cx: &mut Context<Self>) {
        self.updating = updating;
        cx.notify();
    }

    /// Set the metadata for the current user
    fn publish(&self, metadata: &Metadata, cx: &App) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let metadata = metadata.clone();

        cx.background_spawn(async move {
            // Build and sign the metadata event
            let builder = EventBuilder::metadata(&metadata);
            let event = client.sign_event_builder(builder).await?;

            // Send event to user's relays
            client.send_event(&event).await?;

            Ok(())
        })
    }

    fn update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

                        // Update current panel
                        this.set_updating(false, cx);
                        this.set_profile(window, cx);

                        window.push_notification("Profile updated successfully", cx);
                    })?;
                }
                Err(e) => {
                    cx.update(|window, cx| {
                        window.push_notification(Notification::error(e.to_string()), cx);
                    })?;
                }
            };

            Ok(())
        }));
    }
}

impl Panel for ProfilePanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        self.name.clone().into_any_element()
    }
}

impl EventEmitter<PanelEvent> for ProfilePanel {}

impl Focusable for ProfilePanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProfilePanel {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let avatar_input = self.avatar_input.read(cx).value();

        // Get the avatar
        let avatar = if avatar_input.is_empty() {
            "brand/avatar.png"
        } else {
            avatar_input.as_str()
        };

        // Get the public key as short string
        let shorten_pkey = SharedString::from(shorten_pubkey(self.public_key, 8));

        v_flex()
            .p_3()
            .gap_3()
            .w_full()
            .child(
                v_flex()
                    .h_40()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(Avatar::new(avatar).size(rems(4.25)))
                    .child(
                        Button::new("upload")
                            .icon(IconName::PlusCircle)
                            .label("Add an avatar")
                            .xsmall()
                            .ghost()
                            .rounded()
                            .disabled(self.uploading)
                            .loading(self.uploading)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.upload(window, cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().text_muted)
                            .child(SharedString::from("What should people call you?")),
                    )
                    .child(TextInput::new(&self.name_input).bordered(false).small()),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().text_muted)
                            .child(SharedString::from("A short introduction about you:")),
                    )
                    .child(TextInput::new(&self.bio_input).bordered(false).small()),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().text_muted)
                            .child(SharedString::from("Website:")),
                    )
                    .child(TextInput::new(&self.website_input).bordered(false).small()),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().text_muted)
                            .child(SharedString::from("Public Key:")),
                    )
                    .child(
                        h_flex()
                            .h_8()
                            .w_full()
                            .justify_center()
                            .gap_3()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary_background)
                            .text_sm()
                            .text_color(cx.theme().secondary_foreground)
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
                                        this.copy(this.public_key.to_bech32().unwrap(), window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                Button::new("submit")
                    .icon(IconName::CheckCircle)
                    .label("Update")
                    .primary()
                    .small()
                    .font_semibold()
                    .loading(self.updating)
                    .disabled(self.updating)
                    .on_click(cx.listener(move |this, _ev, window, cx| {
                        this.update(window, cx);
                    })),
            )
    }
}
