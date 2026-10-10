use anyhow::Error;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, App, AppContext, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Task, WeakEntity, Window, div, px,
};
use nip29::{GroupCreateOptions, GroupKey, GroupsEvent, GroupsRegistry};
use nostr_sdk::prelude::*;
use settings::AppSettings;
use state::{NostrRegistry, upload};
use theme::{ActiveTheme, TextStyle, Typeset as _, control_radius};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{DockArea, DockPlacement, PanelHandle};
use ui::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use ui::menu::{DropdownMenu, PopupMenuItem};
use ui::switch::Switch;
use ui::{Disableable, Sizable, StyledExt, WindowExtension, h_flex, v_flex};

const SUGGESTED_RELAY: &str = "wss://relay.ditto.pub";
const PRIVATE_DESC: &str = "Only members can read group messages";
const CLOSED_DESC: &str = "Join requests are ignored (invite-only)";
const RESTRICTED_DESC: &str = "Only members can post messages";
const HIDDEN_DESC: &str = "Hidden from non-members, not discoverable";
const LIST_PRIVATE_DESC: &str = "Kept encrypted in your group list so others can't see you joined";

pub fn open(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut App) {
    open_with(dock, None, window, cx);
}

pub fn open_channel(
    dock: WeakEntity<DockArea>,
    parent: GroupKey,
    window: &mut Window,
    cx: &mut App,
) {
    open_with(dock, Some(parent), window, cx);
}

fn open_with(
    dock: WeakEntity<DockArea>,
    parent: Option<GroupKey>,
    window: &mut Window,
    cx: &mut App,
) {
    let is_channel = parent.is_some();
    let view = cx.new(|cx| NewGroup::new(dock, parent, window, cx));

    window.open_dialog(cx, {
        let view = view.clone();

        move |this, _window, _cx| {
            let title = if is_channel {
                "Create Channel"
            } else {
                "Create a Group"
            };

            this.width(px(440.)).title(title).child(view.clone())
        }
    });
}

struct NewGroup {
    dock: WeakEntity<DockArea>,
    /// The parent group when creating a channel; the relay is locked to its own.
    parent: Option<GroupKey>,
    name_input: Entity<InputState>,
    id_input: Entity<InputState>,
    about_input: Entity<TextareaState>,
    custom_relay_input: Entity<InputState>,
    /// The relays offered in the dropdown.
    relays: Vec<RelayUrl>,
    /// The selected relay; `None` selects the custom relay field.
    selected_relay: Option<RelayUrl>,
    /// Whether the optional custom group id input is shown.
    show_custom_id: bool,
    /// Keep the group in the NIP-44 encrypted section of the user's group list.
    list_private: bool,
    /// The uploaded group picture, ready to be published with the metadata.
    picture: Option<SharedString>,
    /// Whether a picture upload is in flight.
    uploading: bool,
    private: bool,
    closed: bool,
    restricted: bool,
    hidden: bool,
    creating: bool,
    error: Option<SharedString>,
    tasks: Vec<Task<Result<(), Error>>>,
    _subscriptions: Vec<Subscription>,
}

impl NewGroup {
    fn new(
        dock: WeakEntity<DockArea>,
        parent: Option<GroupKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("#example"));
        let id_input = cx.new(|cx| InputState::new(window, cx).placeholder("my-group"));

        let custom_relay_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("relay.example.com"));

        let about_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 5)
                .placeholder("What is this group about?")
        });

        let relays = match RelayUrl::parse(SUGGESTED_RELAY) {
            Ok(relay) => vec![relay],
            Err(error) => {
                log::warn!("the suggested group relay is invalid: {error}");
                Vec::new()
            }
        };
        let selected_relay = relays.first().cloned();

        let registry = GroupsRegistry::global(cx);
        let mut subscriptions = vec![];

        subscriptions.push(cx.subscribe_in(
            &registry,
            window,
            |this, _registry, event, window, cx| match event {
                GroupsEvent::Created { key, .. } => {
                    this.creating = false;
                    this.open_group(key.clone(), window, cx);
                }
                GroupsEvent::Error(error) if this.creating => {
                    this.creating = false;
                    this.set_error(friendly_error(error), cx);
                }
                _ => {}
            },
        ));

        // Clear stale errors as the user edits the failing fields.
        for input in [&name_input, &custom_relay_input] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _input, event, _window, cx| {
                    if let InputEvent::Change = event {
                        this.clear_error(cx);
                    }
                },
            ));
        }

        // Load the user's relays once the view is mounted; a channel's relay
        // is locked to its parent's, so there is nothing to pick.
        if parent.is_none() {
            cx.defer_in(window, |this, window, cx| {
                this.load_relays(window, cx);
            });
        }

        Self {
            dock,
            parent,
            name_input,
            id_input,
            about_input,
            custom_relay_input,
            relays,
            selected_relay,
            show_custom_id: false,
            list_private: false,
            picture: None,
            uploading: false,
            private: false,
            closed: false,
            restricted: false,
            hidden: false,
            creating: false,
            error: None,
            tasks: vec![],
            _subscriptions: subscriptions,
        }
    }

    /// Load the signed-in user's relay list.
    fn load_relays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let task: Task<Result<Vec<RelayUrl>, Error>> = cx.background_spawn(async move {
            let filter = Filter::new()
                .kind(Kind::RelayList)
                .author(public_key)
                .limit(1);

            if let Some(event) = client.database().query(filter).await?.into_iter().next() {
                Ok(nip65::extract_relay_list(&event)
                    .map(|(url, _)| url)
                    .collect())
            } else {
                Ok(Vec::new())
            }
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let relays = task.await?;

            this.update(cx, |this, cx| {
                if !relays.is_empty() {
                    this.relays = relays;
                }

                this.selected_relay = this
                    .selected_relay
                    .take()
                    .filter(|selected| this.relays.contains(selected))
                    .or_else(|| this.relays.first().cloned());

                cx.notify();
            })?;

            Ok(())
        }));
    }

    /// The relay the group will be created on.
    fn effective_relay(&self, cx: &App) -> Result<RelayUrl, SharedString> {
        let Some(selected) = &self.selected_relay else {
            let raw = self.custom_relay_input.read(cx).value().trim().to_owned();

            if raw.is_empty() {
                return Err("Enter a relay to create the group on.".into());
            }

            let url = if raw.starts_with("wss://") || raw.starts_with("ws://") {
                raw
            } else {
                format!("wss://{raw}")
            };

            return RelayUrl::parse(&url).map_err(|_| {
                SharedString::from("Enter a valid relay URL (e.g. relay.example.com).")
            });
        };

        Ok(selected.clone())
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        if self.creating || self.uploading {
            return;
        }

        let registry = GroupsRegistry::global(cx);
        let name = self.name_input.read(cx).value().trim().to_owned();

        if name.is_empty() {
            self.set_error("Group name is required.", cx);
            return;
        }

        let relay = match &self.parent {
            Some(parent) => parent.relay().clone(),
            None => match self.effective_relay(cx) {
                Ok(relay) => relay,
                Err(error) => {
                    self.set_error(error, cx);
                    return;
                }
            },
        };

        let custom_id = self.id_input.read(cx).value().trim().to_lowercase();
        if !custom_id.is_empty()
            && !custom_id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            self.set_error(
                "The group id can only contain letters, numbers, '-' and '_'.",
                cx,
            );
            return;
        }

        let about = self.about_input.read(cx).value().trim().to_owned();

        let options = GroupCreateOptions {
            name,
            about: (!about.is_empty()).then_some(about),
            picture: self.picture.as_ref().map(|picture| picture.to_string()),
            private: self.private,
            closed: self.closed,
            restricted: self.restricted,
            hidden: self.hidden,
            list_private: self.list_private,
            custom_id: (!custom_id.is_empty()).then_some(custom_id),
            parent: self.parent.as_ref().map(|parent| parent.id().clone()),
        };

        self.creating = true;
        self.error = None;
        cx.notify();

        registry.update(cx, |registry, cx| {
            registry.create(relay, options, cx);
        });
    }

    fn open_group(&self, key: GroupKey, window: &mut Window, cx: &mut App) {
        let registry = GroupsRegistry::global(cx);
        let group = registry.update(cx, |registry, cx| registry.open(key, cx));

        ui::dock::add_panel_to(
            &self.dock,
            PanelHandle::new(nip29_ui::init(group, window, cx)),
            DockPlacement::Center,
            window,
            cx,
        );

        window.close_dialog(cx);
    }

    fn select_relay(&mut self, relay: Option<RelayUrl>, cx: &mut Context<Self>) {
        self.selected_relay = relay;
        self.error = None;
        cx.notify();
    }

    fn set_uploading(&mut self, uploading: bool, cx: &mut Context<Self>) {
        self.uploading = uploading;
        cx.notify();
    }

    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server = AppSettings::get_file_server(cx);

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

            match upload(server, path, cx).await {
                Ok(url) => {
                    this.update(cx, |this, cx| {
                        this.picture = Some(url.to_string().into());
                        this.set_uploading(false, cx);
                    })?;
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.set_uploading(false, cx);
                        this.set_error(friendly_error(&error.to_string()), cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    fn set_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }

    fn render_relay(&self, selected: &str, cx: &mut Context<Self>) -> impl IntoElement {
        // A channel must live on its parent's relay: the parent tag carries a
        // relay-scoped group id the relay validates against its own state.
        if let Some(parent) = &self.parent {
            return v_flex().gap_1().child(field_label("Relay", cx)).child(
                div()
                    .text_style(TextStyle::Caption)
                    .text_color(cx.theme().text_muted)
                    .child(format!(
                        "Created on {} (same relay as the parent group).",
                        relay_label(parent.relay())
                    )),
            );
        }

        let this = cx.entity().downgrade();
        let relays = self.relays.clone();

        v_flex()
            .gap_1()
            .child(field_label("Relay", cx))
            .when(self.selected_relay.is_none(), |this| {
                this.child(Input::new(&self.custom_relay_input))
            })
            .child(
                Button::new("relay")
                    .label(selected.to_owned())
                    .ghost_alt()
                    .caret()
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _window, _cx| {
                        let menu = relays.iter().fold(menu, |menu, relay| {
                            let this = this.clone();
                            let relay = relay.clone();

                            menu.item(PopupMenuItem::new(relay_label(&relay)).on_click(
                                move |_, _window, cx| {
                                    this.update(cx, |this, cx| {
                                        this.select_relay(Some(relay.clone()), cx);
                                    })
                                    .ok();
                                },
                            ))
                        });

                        let this = this.clone();

                        menu.item(PopupMenuItem::new("Custom relay…").on_click(
                            move |_, _window, cx| {
                                this.update(cx, |this, cx| this.select_relay(None, cx)).ok();
                            },
                        ))
                    }),
            )
    }

    fn render_access(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let private = Switch::new("private")
            .label("Private")
            .description(PRIVATE_DESC)
            .checked(self.private)
            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                this.private = *checked;
                cx.notify();
            }));

        let closed = Switch::new("closed")
            .label("Closed")
            .description(CLOSED_DESC)
            .checked(self.closed)
            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                this.closed = *checked;
                cx.notify();
            }));

        let restricted = Switch::new("restricted")
            .label("Restricted")
            .description(RESTRICTED_DESC)
            .checked(self.restricted)
            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                this.restricted = *checked;
                cx.notify();
            }));

        let hidden = Switch::new("hidden")
            .label("Hidden")
            .description(HIDDEN_DESC)
            .checked(self.hidden)
            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                this.hidden = *checked;
                cx.notify();
            }));

        v_flex()
            .gap_2()
            .child(field_label("ACCESS SETTINGS", cx))
            .child(switch_row(private, cx))
            .child(switch_row(closed, cx))
            .child(switch_row(restricted, cx))
            .child(switch_row(hidden, cx))
    }

    fn render_list_setting(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let list_private = Switch::new("list-private")
            .label("Private on my list")
            .description(LIST_PRIVATE_DESC)
            .checked(self.list_private)
            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                this.list_private = *checked;
                cx.notify();
            }));

        v_flex()
            .gap_2()
            .child(field_label("YOUR LIST", cx))
            .child(switch_row(list_private, cx))
    }

    fn render_error(
        &self,
        error: &str,
        web_url: Option<&str>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_style(TextStyle::Caption)
                    .text_color(cx.theme().danger)
                    .child(error.to_owned()),
            )
            .when(shows_relay_website(error), |this| {
                this.child(
                    div()
                        .id("relay-website")
                        .cursor_pointer()
                        .flex_shrink_0()
                        .underline()
                        .text_style(TextStyle::Caption)
                        .text_color(cx.theme().danger)
                        .child("Open relay website →")
                        .on_click({
                            let web_url = web_url.map(str::to_owned);

                            move |_, _window, cx| {
                                if let Some(url) = &web_url {
                                    cx.open_url(url);
                                }
                            }
                        }),
                )
            })
    }
}

impl Render for NewGroup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self
            .selected_relay
            .as_ref()
            .map(relay_label)
            .unwrap_or_else(|| "Custom relay…".into());

        let web_url = self
            .effective_relay(cx)
            .ok()
            .map(|relay| relay_web_url(&relay));

        // The avatar fallback is seeded with the id, falling back to the name.
        let id = self.id_input.read(cx).value().trim().to_lowercase();
        let name = self.name_input.read(cx).value().trim().to_owned();
        let seed: String = if !id.is_empty() {
            id
        } else if !name.is_empty() {
            name
        } else {
            "group".to_owned()
        };

        v_flex()
            .gap_3()
            .child(
                div()
                    .text_style(TextStyle::Caption)
                    .text_color(cx.theme().text_muted)
                    .child(
                        "Give your new group a name and description. You can always change these later.",
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .child(Avatar::new(self.picture.clone()).seed(seed).large())
                    .child(
                        Button::new("upload-picture")
                            .label(if self.picture.is_some() {
                                "Change picture"
                            } else {
                                "Add a picture"
                            })
                            .xsmall()
                            .ghost()
                            .disabled(self.uploading)
                            .loading(self.uploading)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.upload(window, cx);
                            })),
                    ),
            )
            .child(field("Group Name", Input::new(&self.name_input), cx))
            .when(self.show_custom_id, |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(field("Group ID (optional)", Input::new(&self.id_input), cx))
                        .child(
                            div()
                                .text_style(TextStyle::Caption)
                                .text_color(cx.theme().text_faint)
                                .child(
                                    "Leave empty for a random ID. The relay may override your choice.",
                                ),
                        ),
                )
            })
            .when(!self.show_custom_id, |this| {
                this.child(
                    div()
                        .id("show-custom-id")
                        .cursor_pointer()
                        .underline()
                        .text_style(TextStyle::Caption)
                        .text_color(cx.theme().accent)
                        .child("Set a custom ID")
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.show_custom_id = true;
                            cx.notify();
                        })),
                )
            })
            .child(self.render_relay(&selected, cx))
            .child(field("Description", Textarea::new(&self.about_input), cx))
            .child(self.render_access(cx))
            .child(self.render_list_setting(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(self.render_error(&error, web_url.as_deref(), cx))
            })
            .child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("create")
                            .label(if self.parent.is_some() {
                                "Create Channel"
                            } else {
                                "Create Group"
                            })
                            .primary()
                            .font_semibold()
                            .loading(self.creating)
                            .disabled(self.creating || self.uploading)
                            .on_click(cx.listener(|this, _event, _window, cx| this.create(cx))),
                    ),
            )
    }
}

fn field(text: &'static str, input: impl IntoElement, cx: &App) -> Div {
    v_flex().gap_1().child(field_label(text, cx)).child(input)
}

fn field_label(text: &'static str, cx: &App) -> Div {
    div()
        .text_style(TextStyle::Caption)
        .text_color(cx.theme().text_muted)
        .font_semibold()
        .child(text)
}

fn switch_row(switch: impl IntoElement, cx: &App) -> Div {
    div()
        .w_full()
        .p_2()
        .rounded(px(control_radius()))
        .bg(cx.theme().surface_raised)
        .child(switch)
}

fn relay_label(relay: &RelayUrl) -> SharedString {
    relay
        .as_str()
        .trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .to_owned()
        .into()
}

fn relay_web_url(relay: &RelayUrl) -> String {
    relay
        .as_str()
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1)
}

/// Whether the relay's website likely resolves the rejection.
fn shows_relay_website(error: &str) -> bool {
    let error = error.to_lowercase();
    [
        "website",
        "not allowed",
        "restricted",
        "authorization",
        "auth-required",
        "blocked",
    ]
    .iter()
    .any(|hint| error.contains(hint))
}

/// Map raw relay rejection reasons to user-facing copy (nostrord parity).
fn friendly_error(raw: &str) -> SharedString {
    let lowered = raw.to_lowercase();

    if lowered.contains("blocked:") {
        "Group creation on this relay must be done via the relay's website.".into()
    } else if lowered.contains("auth-required")
        || lowered.contains("not allowed")
        || lowered.contains("restricted")
    {
        "This relay requires authorization to create groups.".into()
    } else if lowered.contains("did not respond") || lowered.contains("timeout") {
        "Relay did not respond. Try again.".into()
    } else if lowered.contains("not connected") {
        "Not connected to relay. Try again.".into()
    } else {
        raw.into()
    }
}
