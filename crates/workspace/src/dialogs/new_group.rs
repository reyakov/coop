use anyhow::Error;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled,
    Subscription, Task, WeakEntity, Window, div, px,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{DockArea, DockPlacement, PanelHandle};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::searchable_list::{SearchableListDelegate, SearchableListItem};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme, Disableable, IndexPath, Sizable, WindowExt, h_flex, v_flex,
};
use nip29::{GroupCreateOptions, GroupKey, GroupsEvent, GroupsRegistry};
use nostr_sdk::prelude::*;
use settings::AppSettings;
use state::{pick_file, upload_file};
use ui::{Avatar, settings_row};

const DEFAULT_RELAYS: [&str; 2] = ["wss://relay.ditto.net", "wss://chat.wisp.talk"];
const PRIVATE_DESC: &str = "Only members can read group messages";
const CLOSED_DESC: &str = "Join requests are ignored (invite-only)";
const RESTRICTED_DESC: &str = "Only members can post messages";
const HIDDEN_DESC: &str = "Hidden from non-members, not discoverable";
const LIST_PRIVATE_DESC: &str = "Kept encrypted in your group list so others can't see";

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

            this.width(px(460.)).title(title).child(view.clone())
        }
    });
}

struct NewGroup {
    dock: WeakEntity<DockArea>,
    /// The parent group when creating a channel; the relay is locked to its own.
    parent: Option<GroupKey>,
    name_input: Entity<InputState>,
    about_input: Entity<TextareaState>,
    custom_relay_input: Entity<InputState>,
    /// The selected relay; `None` selects the custom relay field.
    selected_relay: Option<RelayUrl>,
    /// The relay select; its options end with the custom relay choice.
    relay_select: Entity<SelectState<RelayDelegate>>,
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

        let custom_relay_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("relay.example.com"));

        let about_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 5)
                .placeholder("What is this group about?")
        });

        let relays: Vec<RelayUrl> = DEFAULT_RELAYS
            .iter()
            .filter_map(|url| match RelayUrl::parse(url) {
                Ok(relay) => Some(relay),
                Err(error) => {
                    log::warn!("the default group relay {url} is invalid: {error}");
                    None
                }
            })
            .collect();
        let selected_relay = relays.first().cloned();

        let mut options: Vec<RelayOption> = relays
            .iter()
            .map(|relay| RelayOption {
                label: relay_label(relay),
                value: Some(relay.clone()),
            })
            .collect();
        options.push(RelayOption {
            label: "Custom relay".into(),
            value: None,
        });

        let registry = GroupsRegistry::global(cx);
        let mut subscriptions = vec![];

        let delegate = RelayDelegate { options };
        let selected_index = delegate.position(&selected_relay);
        let relay_select = cx.new(|cx| SelectState::new(delegate, selected_index, window, cx));

        subscriptions.push(cx.subscribe_in(
            &relay_select,
            window,
            |this, _select, event, _window, cx| {
                let SelectEvent::Confirm(value) = event;

                if let Some(choice) = value {
                    this.select_relay(choice.clone(), cx);
                }
            },
        ));

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

        Self {
            dock,
            parent,
            name_input,
            about_input,
            custom_relay_input,
            selected_relay,
            relay_select,
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
            custom_id: None,
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

        crate::dock::add_panel_to(
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

        let picker = pick_file(cx);

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            // Selecting no file means the prompt was cancelled
            let Some(picked) = picker.await? else {
                return Ok(());
            };

            this.update(cx, |this, cx| {
                this.set_uploading(true, cx);
            })?;

            match upload_file(server, picked, cx).await {
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

    fn render_relay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(parent) = &self.parent {
            return v_flex().gap_1p5().child(field_label("Relay", cx)).child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Created on {} (same relay as the parent group).",
                        relay_label(parent.relay())
                    )),
            );
        }

        v_flex()
            .gap_1p5()
            .child(field_label("Relay", cx))
            .when(self.selected_relay.is_none(), |this| {
                this.child(Input::new(&self.custom_relay_input))
            })
            .child(Select::new(&self.relay_select).accessibility_label("Relay"))
    }

    fn render_access(&self, cx: &mut Context<Self>) -> impl IntoElement {
        GroupBox::new()
            .fill()
            .title("Access settings")
            .title_style(group_box_title_style(cx))
            .child(settings_row(
                "Private",
                PRIVATE_DESC,
                Switch::new("private")
                    .checked(self.private)
                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                        this.private = *checked;
                        cx.notify();
                    })),
                cx,
            ))
            .child(settings_row(
                "Closed",
                CLOSED_DESC,
                Switch::new("closed")
                    .checked(self.closed)
                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                        this.closed = *checked;
                        cx.notify();
                    })),
                cx,
            ))
            .child(settings_row(
                "Restricted",
                RESTRICTED_DESC,
                Switch::new("restricted")
                    .checked(self.restricted)
                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                        this.restricted = *checked;
                        cx.notify();
                    })),
                cx,
            ))
            .child(settings_row(
                "Hidden",
                HIDDEN_DESC,
                Switch::new("hidden")
                    .checked(self.hidden)
                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                        this.hidden = *checked;
                        cx.notify();
                    })),
                cx,
            ))
    }

    fn render_list_setting(&self, cx: &mut Context<Self>) -> impl IntoElement {
        GroupBox::new()
            .fill()
            .title("List")
            .title_style(group_box_title_style(cx))
            .child(settings_row(
                "Private on my list",
                LIST_PRIVATE_DESC,
                Switch::new("list-private")
                    .checked(self.list_private)
                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                        this.list_private = *checked;
                        cx.notify();
                    })),
                cx,
            ))
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
                    .text_xs()
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
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child("Open relay website")
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
        let web_url = self
            .effective_relay(cx)
            .ok()
            .map(|relay| relay_web_url(&relay));

        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .id("group-dialog-content")
                    .h(px(500.))
                    .gap_3()
                    .when_some(self.error.clone(), |this, error| {
                        this.child(self.render_error(&error, web_url.as_deref(), cx))
                    })
                    .child(
                        h_flex()
                            .items_center()
                            .gap_3()
                            .child(
                                Avatar::new(self.picture.clone())
                                    .seed(self.name_input.read(cx).value().trim().to_owned())
                                    .large(),
                            )
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
                    .child(input_field("Name", Input::new(&self.name_input), cx))
                    .child(self.render_relay(cx))
                    .child(input_field("About", Textarea::new(&self.about_input), cx))
                    .child(self.render_access(cx))
                    .child(self.render_list_setting(cx))
                    .overflow_y_scroll(),
            )
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
                            .loading(self.creating)
                            .disabled(self.creating || self.uploading)
                            .on_click(cx.listener(|this, _event, _window, cx| this.create(cx))),
                    ),
            )
    }
}

/// One choice in the relay select: a fixed relay, or the custom relay field.
#[derive(Clone)]
struct RelayOption {
    label: SharedString,
    value: Option<RelayUrl>,
}

impl SearchableListItem for RelayOption {
    type Value = Option<RelayUrl>;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }

    /// The trigger reads "Add relay" while the custom relay field is active.
    fn display_title(&self) -> Option<AnyElement> {
        (self.value.is_none()).then(|| "Add relay".into_any_element())
    }
}

/// Relay options with a separator before the trailing custom relay choice.
struct RelayDelegate {
    options: Vec<RelayOption>,
}

impl SearchableListDelegate for RelayDelegate {
    type Item = RelayOption;

    fn items_count(&self, _section: usize) -> usize {
        self.options.len()
    }

    fn item(&self, ix: IndexPath) -> Option<&RelayOption> {
        self.options.get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.options
            .iter()
            .position(|option| option.value() == value)
            .map(IndexPath::new)
    }

    fn render_item(
        &self,
        _ix: IndexPath,
        item: &RelayOption,
        _checked: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        (item.value.is_none()).then(|| {
            v_flex()
                .w_full()
                .gap_1()
                .child(Separator::horizontal())
                .child(item.title())
                .into_any_element()
        })
    }
}

fn input_field(text: &'static str, input: impl IntoElement, cx: &App) -> Div {
    v_flex().gap_1p5().child(field_label(text, cx)).child(input)
}

fn field_label(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .font_semibold()
        .child(text)
}

/// Match [`field_label`]'s design for GroupBox titles.
fn group_box_title_style(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .font_semibold()
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
