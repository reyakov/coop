use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, Context, ElementId,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ObjectFit,
    ParentElement as _, RenderOnce, SharedString, SharedUri, StatefulInteractiveElement as _,
    StyleRefinement, Styled, StyledImage, Window, div, img, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme, Disableable, InteractiveElementExt as _, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use smallvec::SmallVec;

type MouseDownListener = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;
type ClickListener = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type SharedClickListener = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type DropdownBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// Messages from the same author within this window are grouped into one run.
pub const RUN_WINDOW_SECS: u64 = 300;

/// Whether a message starts a new run, given the previous message's author and unix time.
pub fn opens_run<T: PartialEq>(previous: Option<(T, u64)>, current: (T, u64)) -> bool {
    match previous {
        None => true,
        Some((previous_author, previous_at)) => {
            previous_author != current.0 || current.1.saturating_sub(previous_at) > RUN_WINDOW_SECS
        }
    }
}

/// A single message row in a chat-like list.
#[derive(IntoElement)]
pub struct MessageRow {
    id: ElementId,
    style: StyleRefinement,
    show_author: bool,
    hide_avatar: bool,
    avatar: Option<AnyElement>,
    author: Option<SharedString>,
    timestamp: Option<SharedString>,
    header_extras: SmallVec<[AnyElement; 1]>,
    body: SmallVec<[AnyElement; 4]>,
    overlays: SmallVec<[AnyElement; 2]>,
    on_mouse_down: Option<(MouseButton, MouseDownListener)>,
    on_double_click: Option<ClickListener>,
}

impl MessageRow {
    /// Create a message row with the given element id.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            show_author: false,
            hide_avatar: false,
            avatar: None,
            author: None,
            timestamp: None,
            header_extras: SmallVec::new(),
            body: SmallVec::new(),
            overlays: SmallVec::new(),
            on_mouse_down: None,
            on_double_click: None,
        }
    }

    /// Whether this row opens a run of messages from one author.
    #[must_use]
    pub fn show_author(mut self, show_author: bool) -> Self {
        self.show_author = show_author;
        self
    }

    /// Hide the avatar column entirely.
    #[must_use]
    pub fn hide_avatar(mut self, hide_avatar: bool) -> Self {
        self.hide_avatar = hide_avatar;
        self
    }

    /// The avatar shown when this row opens a run of messages.
    #[must_use]
    pub fn avatar(mut self, avatar: impl IntoElement) -> Self {
        self.avatar = Some(avatar.into_any_element());
        self
    }

    /// The author's display name, shown next to the timestamp.
    #[must_use]
    pub fn author(mut self, author: impl Into<SharedString>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// The time the message was sent.
    #[must_use]
    pub fn timestamp(mut self, timestamp: impl Into<SharedString>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    /// Append an element to the header row.
    #[must_use]
    pub fn header_extra(mut self, extra: impl IntoElement) -> Self {
        self.header_extras.push(extra.into_any_element());
        self
    }

    /// Append an element to the message body, in order.
    #[must_use]
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.body.push(child.into_any_element());
        self
    }

    /// Append several elements to the message body, in order.
    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.body
            .extend(children.into_iter().map(|child| child.into_any_element()));
        self
    }

    /// Append an element positioned over the row.
    #[must_use]
    pub fn overlay(mut self, overlay: impl IntoElement) -> Self {
        self.overlays.push(overlay.into_any_element());
        self
    }

    /// Handle a mouse button press anywhere on the row.
    #[must_use]
    pub fn on_mouse_down(
        mut self,
        button: MouseButton,
        listener: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_mouse_down = Some((button, Box::new(listener)));
        self
    }

    /// Handle a double click anywhere on the row.
    #[must_use]
    pub fn on_double_click(
        mut self,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_double_click = Some(Box::new(listener));
        self
    }
}

impl Styled for MessageRow {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for MessageRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            style,
            show_author,
            hide_avatar,
            avatar,
            author,
            timestamp,
            header_extras,
            body,
            overlays,
            on_mouse_down,
            on_double_click,
        } = self;

        div()
            .id(id)
            .group("")
            .relative()
            .w_full()
            .py_0p5()
            .when(show_author, |this| this.pt_2())
            .px_3()
            .refine_style(&style)
            .child(
                h_flex()
                    .items_start()
                    .gap_3()
                    .when(!hide_avatar, |this| {
                        if show_author {
                            match avatar {
                                Some(avatar) => this.child(avatar),
                                None => this.child(div().flex_shrink_0().w(px(24.))),
                            }
                        } else {
                            this.child(div().flex_shrink_0().w(px(24.)))
                        }
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .w_full()
                            .min_w_0()
                            .flex_initial()
                            .overflow_hidden()
                            .text_size(px(15.))
                            .when(show_author, |this| {
                                this.child(
                                    h_flex()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .when_some(author, |this, author| {
                                            this.child(div().font_semibold().child(author))
                                        })
                                        .when_some(timestamp, |this, timestamp| {
                                            this.child(timestamp)
                                        })
                                        .children(header_extras),
                                )
                            })
                            .children(body),
                    ),
            )
            .children(overlays)
            .when_some(on_mouse_down, |this, (button, listener)| {
                this.on_mouse_down(button, listener)
            })
            .when_some(on_double_click, |this, listener| {
                this.on_double_click(listener)
            })
            .hover(|this| this.bg(cx.theme().list_hover))
            .into_any_element()
    }
}

/// A quoted preview of a message this message replies to.
#[derive(IntoElement)]
pub struct ReplyPreview {
    id: ElementId,
    style: StyleRefinement,
    author: SharedString,
    preview: SharedString,
    on_click: Option<ClickListener>,
}

impl ReplyPreview {
    /// Create a reply preview with the given element id, author and snippet.
    pub fn new(
        id: impl Into<ElementId>,
        author: impl Into<SharedString>,
        preview: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            author: author.into(),
            preview: preview.into(),
            on_click: None,
        }
    }

    /// Handle a click anywhere on the preview.
    #[must_use]
    pub fn on_click(
        mut self,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(listener));
        self
    }
}

impl Styled for ReplyPreview {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ReplyPreview {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            style,
            author,
            preview,
            on_click,
        } = self;

        div()
            .id(id)
            .w_full()
            .px_2()
            .border_l_2()
            .border_color(cx.theme().list_active)
            .text_xs()
            .refine_style(&style)
            .child(div().font_semibold().child(author))
            .child(div().w_full().text_ellipsis().line_clamp(1).child(preview))
            .hover(|this| this.bg(cx.theme().list_hover))
            .when_some(on_click, |this, on_click| this.on_click(on_click))
            .into_any_element()
    }
}

/// The image media of a message: one large image or a wrapping row of thumbnails.
#[derive(IntoElement)]
pub struct MessageMedia {
    style: StyleRefinement,
    media: Vec<SharedUri>,
}

impl MessageMedia {
    /// Create media from image URLs.
    pub fn new(media: &[SharedUri]) -> Self {
        Self {
            style: StyleRefinement::default(),
            media: media.to_vec(),
        }
    }
}

impl Styled for MessageMedia {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for MessageMedia {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { style, media } = self;

        if media.is_empty() {
            return div().into_any_element();
        }

        if let [only] = media.as_slice() {
            return div()
                .refine_style(&style)
                .child(
                    img(only.clone())
                        .border_1()
                        .border_color(cx.theme().border)
                        .h(px(250.))
                        .object_fit(ObjectFit::Cover)
                        .rounded(cx.theme().radius_lg),
                )
                .into_any_element();
        }

        div()
            .w_full()
            .flex_1()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .refine_style(&style)
            .children(media.into_iter().enumerate().map(|(ix, item)| {
                div().id(format!("media-{ix}")).flex_shrink_0().child(
                    img(item)
                        .h_32()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius_lg),
                )
            }))
            .into_any_element()
    }
}

/// Reactions of one emoji with the count of authors and whether the current user reacted.
pub struct ReactionGroup {
    pub emoji: SharedString,
    /// Image of a custom emoji (NIP-30) reaction
    pub image: Option<SharedUri>,
    pub count: usize,
    pub mine: bool,
}

/// Group (emoji, image, author) triples by emoji, sorted by emoji.
pub fn group_reactions<A: PartialEq>(
    reactions: impl IntoIterator<Item = (SharedString, Option<SharedUri>, A)>,
    me: Option<A>,
) -> Vec<ReactionGroup> {
    let mut groups: BTreeMap<SharedString, (usize, bool, Option<SharedUri>)> = BTreeMap::new();

    for (emoji, image, author) in reactions {
        let group = groups.entry(emoji).or_default();
        group.0 += 1;
        group.1 |= me.as_ref() == Some(&author);
        if group.2.is_none() {
            group.2 = image;
        }
    }

    groups
        .into_iter()
        .map(|(emoji, (count, mine, image))| ReactionGroup {
            emoji,
            image,
            count,
            mine,
        })
        .collect()
}

/// Reaction chips shown under a message.
#[derive(IntoElement)]
pub struct ReactionBar {
    style: StyleRefinement,
    groups: SmallVec<[ReactionGroup; 4]>,
}

impl ReactionBar {
    /// Create a reaction bar from grouped reactions.
    pub fn new(groups: impl IntoIterator<Item = ReactionGroup>) -> Self {
        Self {
            style: StyleRefinement::default(),
            groups: groups.into_iter().collect(),
        }
    }
}

impl Styled for ReactionBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ReactionBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { style, groups } = self;

        h_flex()
            .mt_2()
            .gap_1()
            .refine_style(&style)
            .children(groups.into_iter().map(|group| {
                h_flex()
                    .gap_2()
                    .py_0p5()
                    .px_1()
                    .rounded(cx.theme().radius)
                    .text_xs()
                    .border_1()
                    .when(group.mine, |this| {
                        this.text_color(cx.theme().foreground)
                            .bg(cx.theme().secondary)
                            .border_color(cx.theme().list_active)
                    })
                    .when(!group.mine, |this| this.border_color(cx.theme().border))
                    .when(group.image.is_none(), |this| this.child(group.emoji))
                    .when_some(group.image, |this, image| {
                        this.child(img(image).size(px(16.)).flex_shrink_0())
                    })
                    .child(SharedString::from(group.count.to_string()))
            }))
    }
}

/// A welcome message shown at the top of a message list.
#[derive(IntoElement)]
pub struct WelcomeMessage {
    id: ElementId,
    style: StyleRefinement,
    icon: Option<AnyElement>,
    title: Option<SharedString>,
    message: Option<SharedString>,
}

impl WelcomeMessage {
    /// Create a welcome message with the given element id.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            icon: None,
            title: None,
            message: None,
        }
    }

    /// The icon shown above the title.
    #[must_use]
    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    /// The welcome title.
    #[must_use]
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The welcome body text.
    #[must_use]
    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }
}

impl Styled for WelcomeMessage {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for WelcomeMessage {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            style,
            icon,
            title,
            message,
        } = self;

        v_flex()
            .id(id)
            .w_full()
            .gap_2()
            .p_3()
            .items_center()
            .justify_center()
            .text_center()
            .refine_style(&style)
            .when_some(icon, |this, icon| this.child(icon))
            .child(
                v_flex()
                    .items_center()
                    .justify_center()
                    .text_center()
                    .when_some(title, |this, title| {
                        this.child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(cx.theme().foreground)
                                .child(title),
                        )
                    })
                    .when_some(message, |this, message| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(message),
                        )
                    }),
            )
    }
}

/// Quick reaction emojis offered in message menus.
pub const REACTION_EMOJIS: &[&str] = &["👍", "👎", "😄", "🎉", "😕", "❤️", "🚀", "👀"];

/// Quick reaction emojis shown on a hovered message's action bar.
pub const COMPACT_REACTION_EMOJIS: &[&str] = &["👍", "❤️", "👀"];

/// Fill a popup menu with reactions to a message, offering `emojis` then the custom ones.
///
/// `react` receives the reaction content: an emoji, or a `:shortcode:` for a custom one.
pub fn react_menu(
    menu: PopupMenu,
    emojis: &[&'static str],
    custom: &[String],
    react: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
) -> PopupMenu {
    let menu = emojis.iter().fold(menu, |menu, emoji| {
        let emoji = *emoji;
        let react = react.clone();
        menu.item(PopupMenuItem::new(emoji).on_click(move |_, window, cx| react(emoji, window, cx)))
    });

    if custom.is_empty() {
        return menu;
    }

    custom.iter().fold(menu.separator(), |menu, emoji| {
        let emoji = emoji.clone();
        let react = react.clone();
        menu.item(
            PopupMenuItem::new(emoji.clone())
                .on_click(move |_, window, cx| react(&emoji, window, cx)),
        )
    })
}

/// A hovered message's action bar: quick reactions, reply, copy, read aloud, and an overflow menu.
#[derive(IntoElement)]
pub struct MessageActions {
    style: StyleRefinement,
    reactions: SmallVec<[(&'static str, SharedClickListener); 4]>,
    reply: Option<SharedClickListener>,
    copy: Option<(ElementId, SharedString)>,
    speech: Option<(bool, SharedClickListener)>,
    dropdown: Option<DropdownBuilder>,
}

impl MessageActions {
    /// Create an empty action bar.
    pub fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            reactions: SmallVec::new(),
            reply: None,
            copy: None,
            speech: None,
            dropdown: None,
        }
    }

    /// Add a quick reaction button.
    pub fn reaction(
        mut self,
        emoji: &'static str,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.reactions.push((emoji, Rc::new(handler)));
        self
    }

    /// Add the reply button.
    pub fn reply(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.reply = Some(Rc::new(handler));
        self
    }

    /// Add the copy button for the given value.
    /// The id must be unique per bar, the clipboard component keys its copied state on it.
    pub fn copy(mut self, id: impl Into<ElementId>, value: impl Into<SharedString>) -> Self {
        self.copy = Some((id.into(), value.into()));
        self
    }

    /// Add the read-aloud button, disabled while a message is already being spoken.
    pub fn speech(
        mut self,
        disabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.speech = Some((disabled, Rc::new(handler)));
        self
    }

    /// Fill the bar's overflow menu.
    pub fn dropdown(
        mut self,
        dropdown: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.dropdown = Some(Rc::new(dropdown));
        self
    }
}

impl Default for MessageActions {
    fn default() -> Self {
        Self::new()
    }
}

impl Styled for MessageActions {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for MessageActions {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            style,
            reactions,
            reply,
            copy,
            speech,
            dropdown,
        } = self;

        // The leading separator only separates reactions from the center buttons
        let has_center =
            !reactions.is_empty() && (reply.is_some() || copy.is_some() || speech.is_some());

        h_flex()
            .p_0p5()
            .gap_1()
            .invisible()
            .absolute()
            .right_4()
            .top_neg_2()
            .when(cx.theme().shadow, |this| this.shadow_sm())
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .children(reactions.into_iter().map(|(emoji, handler)| {
                Button::new(emoji)
                    .label(emoji)
                    .tooltip(emoji)
                    .small()
                    .ghost()
                    .on_click(move |event, window, cx| handler(event, window, cx))
            }))
            .when(has_center, |this| {
                this.child(div().flex_shrink_0().h_4().w_px().bg(cx.theme().border))
            })
            .when_some(reply, |this, reply| {
                this.child(
                    Button::new("reply")
                        .icon(IconName::Reply)
                        .tooltip("Reply")
                        .small()
                        .ghost()
                        .on_click(move |event, window, cx| reply(event, window, cx)),
                )
            })
            .when_some(copy, |this, (id, value)| {
                this.child(Clipboard::new(id).value(value).tooltip("Copy").small())
            })
            .when_some(speech, |this, (disabled, speech)| {
                this.child(
                    Button::new("speech")
                        .icon(crate::IconName::Speech)
                        .tooltip("Read aloud")
                        .small()
                        .ghost()
                        .disabled(disabled)
                        .on_click(move |event, window, cx| speech(event, window, cx)),
                )
            })
            .when_some(dropdown, |this, dropdown| {
                this.child(div().flex_shrink_0().h_4().w_px().bg(cx.theme().border))
                    .child(
                        Button::new("more")
                            .icon(IconName::Ellipsis)
                            .small()
                            .ghost()
                            .dropdown_menu(move |menu, window, cx| dropdown(menu, window, cx)),
                    )
            })
            .group_hover("", |this| this.visible())
            .refine_style(&style)
    }
}

/// A message's text revealed word by word, paced by the estimated speech duration.
#[derive(IntoElement)]
pub struct RevealText {
    text: SharedString,
    duration: Duration,
}

impl RevealText {
    pub fn new(text: impl Into<SharedString>, duration: Duration) -> Self {
        Self {
            text: text.into(),
            duration,
        }
    }
}

impl RenderOnce for RevealText {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let words: Vec<SharedString> = self
            .text
            .split_whitespace()
            .map(SharedString::from)
            .collect();
        let total = words.len().max(1) as f32;

        div().flex().flex_wrap().gap_x_1().with_animation(
            "speech-reveal",
            Animation::new(self.duration).with_max_fps(30.),
            move |this, progress| {
                let visible = ((progress * total).ceil() as usize).min(words.len());

                this.children(words.iter().enumerate().map(|(ix, word)| {
                    div()
                        .when(ix >= visible, |this| this.invisible())
                        .child(word.clone())
                }))
            },
        )
    }
}
