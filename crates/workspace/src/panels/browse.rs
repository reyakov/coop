use std::ops::Range;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, WeakEntity, Window,
    div, uniform_list,
};
use nip29::{GroupCandidate, GroupKey, GroupsEvent, GroupsRegistry};
use nostr_sdk::prelude::RelayUrl;
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{DockArea, DockPlacement, Panel, PanelEvent, PanelHandle};
use ui::indicator::Indicator;
use ui::input::{Input, InputEvent, InputState};
use ui::notification::Notification;
use ui::{Disableable, Icon, IconName, Sizable, StyledExt, WindowExtension, h_flex, v_flex};

const TITLE: &str = "Browse";
const INPUT_PLACEHOLDER: &str = "wss://relay.example.com";

pub fn init(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut App) -> Entity<BrowsePanel> {
    cx.new(|cx| BrowsePanel::new(dock, window, cx))
}

pub struct BrowsePanel {
    focus_handle: FocusHandle,
    dock: WeakEntity<DockArea>,
    /// The relay input
    input: Entity<InputState>,
    /// The relay currently being browsed.
    relay: Option<RelayUrl>,
    /// The groups the relay has announced so far
    candidates: Vec<GroupCandidate>,
    /// Whether the relay is still answering
    loading: bool,
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl BrowsePanel {
    fn new(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(INPUT_PLACEHOLDER));
        let registry = GroupsRegistry::global(cx);

        let mut subscriptions = smallvec![];

        subscriptions.push(
            cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.browse(window, cx);
                }
            }),
        );

        subscriptions.push(
            cx.subscribe(&registry, |this, _registry, event, cx| match event {
                GroupsEvent::Browsed(candidate) => {
                    this.push_candidate(candidate.as_ref().clone(), cx)
                }
                GroupsEvent::BrowseSettled(relay) => {
                    this.settle(relay.clone(), cx);
                }
                _ => {}
            }),
        );

        Self {
            focus_handle: cx.focus_handle(),
            dock,
            input,
            relay: None,
            candidates: Vec::new(),
            loading: false,
            _subscriptions: subscriptions,
        }
    }

    /// Browse the relay in the input.
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let registry = GroupsRegistry::global(cx);
        let value = self.input.read(cx).value().trim().to_owned();

        if value.is_empty() {
            return;
        }

        let relay = match RelayUrl::parse(&value) {
            Ok(relay) => relay,
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()), cx);
                return;
            }
        };

        self.relay = Some(relay.clone());
        self.candidates.clear();
        self.loading = true;

        registry.update(cx, |registry, cx| {
            registry.browse(relay, cx);
        });

        cx.notify();
    }

    fn push_candidate(&mut self, candidate: GroupCandidate, cx: &mut Context<Self>) {
        if self.relay.as_ref() != Some(candidate.key.relay()) {
            return;
        }

        match self.candidates.iter_mut().find(|e| e.key == candidate.key) {
            Some(existing) => *existing = candidate,
            None => self.candidates.push(candidate),
        }

        cx.notify();
    }

    /// The relay stopped answering: EOSE, a refusal, or the auto-close timeout.
    fn settle(&mut self, relay: RelayUrl, cx: &mut Context<Self>) {
        if self.relay.as_ref() != Some(&relay) {
            return;
        }
        self.loading = false;
        cx.notify();
    }

    /// Join a browsed group and open it.
    fn join(&self, key: GroupKey, window: &mut Window, cx: &mut App) {
        let dock = self.dock.clone();
        let registry = GroupsRegistry::global(cx);

        registry.update(cx, |registry, cx| {
            registry.join_key(key.clone(), None, cx);
        });

        let Some(group) = registry.read(cx).group(&key, cx) else {
            return;
        };

        ui::dock::add_panel_to(
            &dock,
            PanelHandle::new(nip29_ui::init(group, window, cx)),
            DockPlacement::Center,
            window,
            cx,
        );
    }

    fn render_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let hide_avatar = AppSettings::get_hide_avatar(cx);
        let panel = cx.entity().downgrade();

        let registry = GroupsRegistry::global(cx);
        let joined: Vec<GroupKey> = registry
            .read(cx)
            .groups()
            .iter()
            .map(|group| group.read(cx).key().clone())
            .collect();

        self.candidates
            .get(range)
            .into_iter()
            .flatten()
            .map(|candidate| {
                let key = candidate.key.clone();
                let name = SharedString::from(candidate.display_name().to_owned());
                let picture = candidate
                    .display_image()
                    .map(|picture| SharedString::from(picture.to_owned()));
                let seed = key.cache_tag();
                let is_joined = joined.contains(&key);

                let action = if is_joined {
                    Button::new(format!("browse-joined-{seed}"))
                        .label("Joined")
                        .small()
                        .disabled(true)
                        .into_any_element()
                } else {
                    let panel = panel.clone();
                    let clicked = key.clone();

                    Button::new(format!("browse-join-{seed}"))
                        .label("Join")
                        .small()
                        .on_click(move |_event, window, cx| {
                            if let Err(error) = panel.update(cx, |panel, cx| {
                                panel.join(clicked.clone(), window, cx);
                            }) {
                                log::error!("Failed to join a browsed group: {error}");
                            }
                        })
                        .into_any_element()
                };

                h_flex()
                    .w_full()
                    .gap_2()
                    .px_1p5()
                    .py_1()
                    .when(!hide_avatar, |this| {
                        this.child(
                            Avatar::from_source(picture)
                                .seed(seed)
                                .small()
                                .flex_shrink_0(),
                        )
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().text_placeholder)
                                    .truncate()
                                    .child(SharedString::from(key.id().as_str().to_owned())),
                            ),
                    )
                    .child(action)
                    .into_any_element()
            })
            .collect()
    }
}

fn notice(text: &'static str, cx: &App) -> AnyElement {
    h_flex()
        .size_full()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().text_muted)
        .child(text)
        .into_any_element()
}

impl Panel for BrowsePanel {
    fn panel_id(&self) -> SharedString {
        TITLE.into()
    }

    fn title(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_1p5()
            .child(
                Icon::new(IconName::Compass)
                    .small()
                    .text_color(cx.theme().icon_muted),
            )
            .child(TITLE)
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for BrowsePanel {}

impl Focusable for BrowsePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowsePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.candidates.len();

        let body = if self.relay.is_none() {
            notice("Enter a relay to browse its groups.", cx)
        } else if count > 0 {
            uniform_list(
                "browse-groups",
                count,
                cx.processor(|this, range, _window, cx| this.render_rows(range, cx)),
            )
            .h_full()
            .into_any_element()
        } else if self.loading {
            notice("Loading groups…", cx)
        } else {
            notice("No groups found.", cx)
        };

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .gap_2()
                    .p_3()
                    .flex_shrink_0()
                    .child(Input::new(&self.input).cleanable(true).flex_1())
                    .child(
                        Button::new("browse-go")
                            .label("Go")
                            .primary()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.browse(window, cx);
                            })),
                    )
                    .when(self.loading, |this| {
                        this.child(Indicator::new().small().color(cx.theme().icon_accent))
                    }),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}
