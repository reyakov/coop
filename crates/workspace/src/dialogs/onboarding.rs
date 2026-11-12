use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext, Context, Div, Entity, Hsla, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    div, px,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{Confirm, DialogButtonProps, DialogFooter};
use gpui_kit::component::input::{InputEvent, InputState, NumberInput};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::{ThemeColor, try_parse_color};
use gpui_kit::component::{
    ActiveTheme, Theme, ThemeConfig, ThemeRegistry, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use rand::seq::IndexedRandom;
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use ui::{settings_block, settings_row};

use super::settings::{FONT_SIZE_RANGE, RADIUS_RANGE};

/// Open the first-run onboarding dialog
pub fn open(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| Onboarding::new(window, cx));

    window.open_dialog(cx, move |this, _window, _cx| {
        this.width(px(640.))
            .close_button(false)
            .overlay_closable(false)
            .keyboard(false)
            .title("Welcome to Coop")
            .child(view.clone())
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Get started")
                    .show_cancel(false)
                    .on_ok(|_, _, cx| {
                        AppSettings::update_onboarded(true, cx);
                        // Returning true closes the dialog
                        true
                    }),
            )
            .footer(
                DialogFooter::new().child(
                    Button::new("get-started")
                        .label("Get started")
                        .primary()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(Confirm { secondary: false }), cx);
                        }),
                ),
            )
    });
}

pub struct Onboarding {
    font_size_input: Entity<InputState>,
    radius_input: Entity<InputState>,
    light_themes: Vec<Rc<ThemeConfig>>,
    dark_themes: Vec<Rc<ThemeConfig>>,
    _subscriptions: SmallVec<[Subscription; 3]>,
}

impl Onboarding {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = AppSettings::get_theme(cx);
        let (light_themes, dark_themes) = Self::sample_themes(cx);

        let font_size_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(theme.font_size.to_string())
                .step(1.)
                .min(FONT_SIZE_RANGE.0 as f64)
                .max(FONT_SIZE_RANGE.1 as f64)
        });

        let radius_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(
                    theme
                        .radius
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
                .placeholder("System")
                .step(1.)
                .min(RADIUS_RANGE.0 as f64)
                .max(RADIUS_RANGE.1 as f64)
        });

        let mut subscriptions = smallvec![];

        subscriptions.push({
            let state = font_size_input.clone();
            cx.subscribe(&state, move |_, state, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }

                if let Some(font_size) = Self::number_at(&state, FONT_SIZE_RANGE, cx) {
                    AppSettings::update_theme(|theme| theme.font_size = font_size, cx);
                    AppSettings::apply_theme_style(cx);
                }
            })
        });

        // One radius control tunes both radii so the whole UI stays coherent
        subscriptions.push({
            let state = radius_input.clone();
            cx.subscribe(&state, move |_, state, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }

                let radius = Self::number_at(&state, RADIUS_RANGE, cx);
                AppSettings::update_theme(
                    move |theme| {
                        theme.radius = radius;
                        theme.radius_lg = radius;
                    },
                    cx,
                );
                AppSettings::apply_theme_style(cx);
            })
        });

        Self {
            font_size_input,
            radius_input,
            light_themes,
            dark_themes,
            _subscriptions: subscriptions,
        }
    }

    /// The number in a theme input, clamped to the range, empty is None
    fn number_at(state: &Entity<InputState>, range: (f32, f32), cx: &App) -> Option<f32> {
        let (min, max) = range;
        state
            .read(cx)
            .value()
            .trim()
            .parse::<f32>()
            .ok()
            .map(|value| value.clamp(min, max))
    }

    /// Three light and three dark bundled themes, sampled at random.
    fn sample_themes(cx: &App) -> (Vec<Rc<ThemeConfig>>, Vec<Rc<ThemeConfig>>) {
        let active = Theme::global(cx).theme_name().clone();
        let mut rng = rand::rng();

        let (lights, darks): (Vec<_>, Vec<_>) = ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .cloned()
            .partition(|theme| !theme.mode.is_dark());

        let mut sample = |group: &[Rc<ThemeConfig>]| {
            let mut picks: Vec<_> = group.sample(&mut rng, 3).cloned().collect();
            let active_config = group.iter().find(|theme| theme.name == active);
            let already_sampled = picks.iter().any(|theme| theme.name == active);

            if let (Some(config), Some(slot)) = (active_config, picks.first_mut())
                && !already_sampled
            {
                *slot = config.clone();
            }

            picks
        };

        (sample(&lights), sample(&darks))
    }

    /// The theme picker: a demo block per sampled bundled theme
    fn themes_view(&self, cx: &App) -> AnyElement {
        let active = Theme::global(cx).theme_name().clone();

        settings_block(
            "Theme",
            "Customize UI colors. More in Settings.",
            v_flex()
                .gap_3()
                .child(Self::theme_group("Light", &self.light_themes, &active, cx))
                .child(Self::theme_group("Dark", &self.dark_themes, &active, cx)),
            cx,
        )
        .into_any_element()
    }

    fn theme_group(
        label: &'static str,
        themes: &[Rc<ThemeConfig>],
        active: &SharedString,
        cx: &App,
    ) -> Div {
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                div().flex().flex_wrap().gap_2().children(
                    themes
                        .iter()
                        .map(|config| Self::theme_card(config, active, cx)),
                ),
            )
    }

    /// A card painting a mini app mockup in the theme's own colors
    fn theme_card(config: &Rc<ThemeConfig>, active: &SharedString, cx: &App) -> impl IntoElement {
        let selected = &config.name == active;
        let name = config.name.clone();
        let preview = Self::theme_preview(config);

        let border = if selected {
            cx.theme().list_active_border
        } else {
            preview.border
        };

        // Square block; the bigger inset keeps the mock window small inside it
        let block = div()
            .flex()
            .w_full()
            .aspect_square()
            .p_4()
            .rounded(cx.theme().radius_lg)
            .overflow_hidden()
            .bg(preview.background)
            .border_1()
            .border_color(border)
            .hover(|this| this.border_color(cx.theme().border));

        let block = if selected { block.border_2() } else { block };

        v_flex()
            .id(SharedString::from(format!("theme-{name}")))
            .flex_1()
            .cursor_pointer()
            .child(
                block.child(
                    h_flex()
                        .size_full()
                        .rounded(cx.theme().radius)
                        .when(cx.theme().shadow, |this| this.shadow_sm())
                        .overflow_hidden()
                        .border_1()
                        .border_color(preview.border)
                        .bg(preview.window)
                        .child(
                            v_flex()
                                .w_1_3()
                                .h_full()
                                .p_1()
                                .justify_between()
                                .bg(preview.sidebar)
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(h_flex().gap_0p5().children((0..3).map(|_| {
                                            div().size_1().rounded_full().bg(preview.accent)
                                        })))
                                        .child(Self::demo_bar(preview.bar))
                                        .child(Self::demo_bar(preview.bar)),
                                )
                                .child(
                                    h_flex()
                                        .gap_0p5()
                                        .items_center()
                                        .child(
                                            div()
                                                .size_2()
                                                .rounded_full()
                                                .bg(preview.bar.opacity(0.55)),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .h_1()
                                                .rounded_full()
                                                .bg(preview.bar.opacity(0.55)),
                                        ),
                                ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .h_full()
                                .p_1()
                                .gap_1()
                                .child(Self::demo_bar(preview.bar))
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .child(div().flex_1().h_4().rounded_sm().bg(preview.block))
                                        .child(div().flex_1().h_4().rounded_sm().bg(preview.block)),
                                ),
                        ),
                ),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .truncate()
                    .text_color(if selected {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(name),
            )
            .on_click({
                let name = config.name.clone();
                move |_, _window, cx| AppSettings::select_theme(&name, cx)
            })
    }

    /// A skeleton text bar for the demo block
    fn demo_bar(color: Hsla) -> Div {
        div().w_full().h_1().rounded_full().bg(color.opacity(0.55))
    }

    /// The demo block colors resolved from a theme config.
    fn theme_preview(config: &ThemeConfig) -> ThemePreview {
        let defaults = if config.mode.is_dark() {
            ThemeColor::dark()
        } else {
            ThemeColor::light()
        };
        let colors = &config.colors;

        let resolve = |value: Option<&SharedString>, fallback: Hsla| {
            value
                .and_then(|value| try_parse_color(value).ok())
                .unwrap_or(fallback)
        };

        ThemePreview {
            background: resolve(colors.background.as_ref(), defaults.background),
            border: resolve(colors.border.as_ref(), defaults.border),
            window: resolve(colors.popover.as_ref(), defaults.popover),
            sidebar: resolve(colors.secondary.as_ref(), defaults.secondary),
            bar: resolve(colors.muted_foreground.as_ref(), defaults.muted_foreground),
            block: resolve(colors.secondary.as_ref(), defaults.secondary),
            accent: resolve(colors.primary.as_ref(), defaults.primary),
        }
    }
}

/// The colors of a theme's demo block
struct ThemePreview {
    background: Hsla,
    border: Hsla,
    window: Hsla,
    sidebar: Hsla,
    bar: Hsla,
    block: Hsla,
    accent: Hsla,
}

impl Render for Onboarding {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const INTRO: &str =
            "Pick how Coop works and looks. You can change all of this later in Preferences.";
        const WOT: &str = "Treat people followed by your contacts as known senders.";
        const NIP4E: &str = "Use a dedicated key to encrypt and decrypt messages.";
        #[cfg(not(target_arch = "wasm32"))]
        const WALLET: &str = "Show the wallet and allow ecash payments.";
        const FONT_SIZE: &str = "The base font size for the interface.";
        const RADIUS: &str =
            "Corner radius for UI elements, including dialogs. Empty follows the theme.";
        const FOCUS_RING: &str = "Draw a ring around focused controls.";
        const SHADOW: &str = "Draw soft shadows on buttons, inputs and dialogs.";

        let wot_auto_trust = AppSettings::get_wot_auto_trust(cx);
        let nip4e = AppSettings::get_nip4e(cx);
        let theme = AppSettings::get_theme(cx);

        let features = v_flex()
            .gap_3()
            .child(settings_row(
                "Web of Trust",
                WOT,
                Switch::new("onboarding-wot")
                    .checked(wot_auto_trust)
                    .on_click(move |checked, _window, cx| {
                        AppSettings::update_wot_auto_trust(*checked, cx);
                    }),
                cx,
            ))
            .child(settings_row(
                "Decoupling Encryption Key",
                NIP4E,
                Switch::new("onboarding-nip4e").checked(nip4e).on_click(
                    move |checked, _window, cx| {
                        AppSettings::update_nip4e(*checked, cx);
                    },
                ),
                cx,
            ));

        #[cfg(not(target_arch = "wasm32"))]
        let features = features.child(settings_row(
            "Wallet",
            WALLET,
            Switch::new("onboarding-wallet")
                .checked(AppSettings::get_wallet(cx))
                .on_click(move |checked, _window, cx| {
                    AppSettings::update_wallet(*checked, cx);
                }),
            cx,
        ));

        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(INTRO),
            )
            .child(features)
            .child(Separator::horizontal())
            .child(self.themes_view(cx))
            .child(Separator::horizontal())
            .child(
                v_flex()
                    .gap_3()
                    .child(settings_row(
                        "Font Size",
                        FONT_SIZE,
                        NumberInput::new(&self.font_size_input).w_24(),
                        cx,
                    ))
                    .child(settings_row(
                        "Corner Radius",
                        RADIUS,
                        NumberInput::new(&self.radius_input).w_24(),
                        cx,
                    ))
                    .child(settings_row(
                        "Focus Ring",
                        FOCUS_RING,
                        Switch::new("onboarding-focus-ring")
                            .checked(theme.focus_ring)
                            .on_click(move |checked, _window, cx| {
                                AppSettings::update_theme(|theme| theme.focus_ring = *checked, cx);
                                AppSettings::apply_theme_style(cx);
                            }),
                        cx,
                    ))
                    .child(settings_row(
                        "Shadows",
                        SHADOW,
                        Switch::new("onboarding-shadow")
                            .checked(theme.shadow)
                            .on_click(move |checked, _window, cx| {
                                AppSettings::update_theme(|theme| theme.shadow = *checked, cx);
                                AppSettings::apply_theme_style(cx);
                            }),
                        cx,
                    )),
            )
    }
}
