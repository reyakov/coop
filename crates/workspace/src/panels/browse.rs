use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, div, px, relative,
    size, transparent_black,
};
use gpui_base::{VirtualListScrollHandle, v_virtual_list};
use nip29::{Discovery, GroupCandidate, GroupKey, GroupsEvent, GroupsRegistry, Membership};
use nostr_sdk::prelude::{RelayUrl, Timestamp};
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use theme::{ActiveTheme, TextStyle, Typeset as _, button_radius, panel_radius};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{DockArea, DockPlacement, Panel, PanelEvent, PanelHandle};
use ui::input::{Input, InputEvent, InputState};
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::{Icon, IconName, Sizable, StyledExt, WindowExtension, h_flex, v_flex};

const TITLE: &str = "Browse";
const INPUT_PLACEHOLDER: &str = "wss://relay.example.com";
const HEADER_HEIGHT: f32 = 96.0;
const CARD_HEIGHT: f32 = 164.0;
const NO_DESCRIPTION: &str = "No description";

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
    /// The groups discovered from the cache and the relays
    discovery: Discovery,
    /// Whether the discovery sync is still running
    discovering: bool,
    /// The scroll position of the panel
    scroll_handle: VirtualListScrollHandle,
    /// Whether the relay is still answering
    loading: bool,
    tasks: SmallVec<[Task<()>; 2]>,
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
                GroupsEvent::Synced => {
                    this.refresh_discovery(cx);
                }
                GroupsEvent::Members {
                    key,
                    created_at,
                    members,
                } => {
                    this.push_members(key.clone(), *created_at, *members, cx);
                }
                _ => {}
            }),
        );

        cx.defer_in(window, |this, _window, cx| {
            this.load_cached(cx);
            this.sync_discovery(cx);
        });

        Self {
            focus_handle: cx.focus_handle(),
            dock,
            input,
            relay: None,
            candidates: Vec::new(),
            discovery: Discovery::default(),
            discovering: true,
            scroll_handle: VirtualListScrollHandle::new(),
            loading: false,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    /// Load the groups from the local database.
    fn load_cached(&mut self, cx: &mut Context<Self>) {
        let registry = GroupsRegistry::global(cx);
        let discovery = registry.read(cx).cached_discovery(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            let discovery = discovery.await;

            this.update(cx, |this, cx| {
                this.discovery = discovery;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Sync the discoverable groups with the relays.
    fn sync_discovery(&mut self, cx: &mut Context<Self>) {
        self.discovering = true;

        let registry = GroupsRegistry::global(cx);
        let sync = registry.read(cx).discover(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            sync.await;

            this.update(cx, |this, cx| {
                this.discovering = false;
                cx.notify();
            })
            .ok();
        }));
    }

    /// A browsed group announced its member list.
    fn push_members(
        &mut self,
        key: GroupKey,
        created_at: Timestamp,
        members: usize,
        cx: &mut Context<Self>,
    ) {
        self.discovery.set_members(key, created_at, members);
        cx.notify();
    }

    /// The discovery synced new groups: re-query the local database.
    fn refresh_discovery(&mut self, cx: &mut Context<Self>) {
        let registry = GroupsRegistry::global(cx);
        let discovery = registry.read(cx).cached_discovery(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            let discovery = discovery.await;

            this.update(cx, |this, cx| {
                this.discovery = discovery;
                cx.notify();
            })
            .ok();
        }));
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
        self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);

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

    /// Open the group's panel to preview (or revisit) it.
    fn open_panel(dock: &WeakEntity<DockArea>, key: GroupKey, window: &mut Window, cx: &mut App) {
        let registry = GroupsRegistry::global(cx);
        let group = registry.update(cx, |registry, cx| registry.open(key, cx));

        ui::dock::add_panel_to(
            dock,
            PanelHandle::new(nip29_ui::init(group, window, cx)),
            DockPlacement::Center,
            window,
            cx,
        );
    }

    /// The discovered groups the user is not a member of.
    fn discovered_cards(&self, cx: &App) -> Vec<GroupCandidate> {
        let member_of: Vec<GroupKey> = GroupsRegistry::global(cx)
            .read(cx)
            .groups()
            .iter()
            .filter(|group| matches!(group.read(cx).membership(), Membership::Member))
            .map(|group| group.read(cx).key().clone())
            .collect();

        self.discovery
            .discovered()
            .iter()
            .filter(|candidate| !member_of.contains(&candidate.key))
            .cloned()
            .collect()
    }

    /// The panel header, item 0 of the list so it scrolls with the cards.
    fn render_header(&self, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .px_4()
            .h_24()
            .flex_shrink_0()
            .justify_between()
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .text_style(TextStyle::Callout)
                            .font_semibold()
                            .line_height(relative(1.2))
                            .child("Public Groups"),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_style(TextStyle::Caption)
                            .text_color(cx.theme().text_muted)
                            .map(|this| match &self.relay {
                                Some(relay) => {
                                    this.child(SharedString::from(relay.as_str().to_owned()))
                                }
                                None => this.child(SharedString::from(
                                    "Hosted on current connected relays",
                                )),
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .w_full()
                    .max_w_64()
                    .px_1()
                    .rounded(px(button_radius()))
                    .bg(cx.theme().surface)
                    .child(
                        Input::new(&self.input)
                            .appearance(false)
                            .small()
                            .flex_1(),
                    )
                    .child(
                        Button::new("relay-browse")
                            .icon(IconName::ArrowRight)
                            .tooltip("Browse by Relay")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.browse(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_list_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let discover = self.relay.is_none();

        let cards: Vec<GroupCandidate> = if discover {
            self.discovered_cards(cx)
        } else {
            self.candidates.clone()
        };

        range
            .map(|ix| {
                if ix == 0 {
                    self.render_header(cx)
                } else {
                    let pair = cards.chunks(2).nth(ix - 1).unwrap_or(&[]);

                    h_flex()
                        .w_full()
                        .h(px(CARD_HEIGHT))
                        .px_4()
                        .pb_4()
                        .gap_4()
                        .children(pair.first().map(|card| self.render_card(card, cx)))
                        .child(match pair.get(1) {
                            Some(card) => self.render_card(card, cx),
                            None => div().flex_1().into_any_element(),
                        })
                        .into_any_element()
                }
            })
            .collect()
    }

    fn render_card(&self, candidate: &GroupCandidate, cx: &mut Context<Self>) -> AnyElement {
        let hide_avatar = AppSettings::get_hide_avatar(cx);
        let dock = self.dock.clone();

        let key = candidate.key.clone();
        let seed = key.cache_tag();
        let members = self.discovery.members(&key).unwrap_or(0);

        let name = SharedString::from(candidate.display_name().to_owned());
        let relay = SharedString::from(key.relay().as_str().to_owned());

        let picture = candidate
            .display_image()
            .map(|picture| SharedString::from(picture.to_owned()));

        let about = candidate
            .display_about()
            .map(|about| SharedString::from(about.to_owned()));

        v_flex()
            .id(SharedString::from(format!("card-{seed}")))
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .rounded(px(panel_radius()))
            .cursor_pointer()
            .border_1()
            .border_color(transparent_black())
            .bg(cx.theme().surface)
            .hover(|this| {
                this.bg(cx.theme().surface_raised)
                    .border_color(cx.theme().border_strong)
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .gap(px(theme::SPACE))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap(px(theme::SPACE))
                            .when(!hide_avatar, |this| {
                                this.child(Avatar::from_source(picture).seed(seed).flex_shrink_0())
                            })
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_style(TextStyle::Callout)
                                            .truncate()
                                            .font_semibold()
                                            .line_height(relative(1.2))
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_style(TextStyle::Caption)
                                            .text_color(cx.theme().text_faint)
                                            .child(relay),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_style(TextStyle::Callout)
                            .text_color(cx.theme().text_muted)
                            .text_ellipsis()
                            .line_clamp(2)
                            .map(|this| {
                                if let Some(about) = about {
                                    this.child(about)
                                } else {
                                    this.child(NO_DESCRIPTION)
                                }
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1p5()
                            .text_color(cx.theme().text_muted)
                            .child(Icon::new(IconName::Book))
                            .child(
                                div()
                                    .text_style(TextStyle::Callout)
                                    .child(SharedString::from(format!("{members} members"))),
                            ),
                    ),
            )
            .on_click(move |_event, window, cx| {
                Self::open_panel(&dock, key.clone(), window, cx);
            })
            .into_any_element()
    }
}

impl Panel for BrowsePanel {
    fn panel_id(&self) -> SharedString {
        TITLE.into()
    }

    fn title(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_1()
            .text_style(TextStyle::Caption)
            .child(
                Icon::new(IconName::Compass)
                    .small()
                    .text_color(cx.theme().text_faint),
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
        let view = cx.entity();
        let discover = self.relay.is_none();

        let cards: Vec<GroupCandidate> = if discover {
            self.discovered_cards(cx)
        } else {
            self.candidates.clone()
        };

        let rows = cards.len().div_ceil(2);

        let content: AnyElement = if rows > 0 {
            let mut item_sizes = vec![size(px(0.), px(HEADER_HEIGHT))];
            item_sizes.resize(rows + 1, size(px(0.), px(CARD_HEIGHT)));

            v_virtual_list(
                view,
                "groups",
                Rc::new(item_sizes),
                |this, range, _window, cx| this.render_list_rows(range, cx),
            )
            .track_scroll(&self.scroll_handle)
            .h_full()
            .into_any_element()
        } else {
            let text = if discover {
                if self.discovering {
                    "Discovering groups…"
                } else {
                    "No groups found."
                }
            } else if self.loading {
                "Loading groups…"
            } else {
                "No groups found."
            };

            v_flex()
                .id("browse-content")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .child(self.render_header(cx))
                .child(notice(text, cx))
                .into_any_element()
        };

        div()
            .size_full()
            .relative()
            .child(content)
            .child(Scrollbar::vertical(&self.scroll_handle))
    }
}

fn notice(text: &'static str, cx: &App) -> AnyElement {
    h_flex()
        .size_full()
        .justify_center()
        .text_style(TextStyle::Callout)
        .text_color(cx.theme().text_muted)
        .child(text)
        .into_any_element()
}
