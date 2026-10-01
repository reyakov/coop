use anyhow::Error;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClipboardItem, Context, Div, IntoElement, ParentElement, Render, SharedString,
    Styled, Task, Window, div, px,
};
use instant::Duration;
use nostr_sdk::prelude::*;
use person::{Person, PersonRegistry, shorten_pubkey};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::{IconName, Sizable, WindowExtension, h_flex, v_flex};

/// Open a read-only profile for `public_key`.
pub fn open(public_key: PublicKey, window: &mut Window, cx: &mut App) {
    let profile = cx.new(|_| ProfileDialog::new(public_key));

    window.open_dialog(cx, move |this, _window, _cx| {
        this.width(px(360.)).title("Profile").child(profile.clone())
    });
}

struct ProfileDialog {
    public_key: PublicKey,
    copied: bool,
    tasks: Vec<Task<Result<(), Error>>>,
}

impl ProfileDialog {
    fn new(public_key: PublicKey) -> Self {
        Self {
            public_key,
            copied: false,
            tasks: vec![],
        }
    }

    fn profile(&self, cx: &App) -> Person {
        let persons = PersonRegistry::global(cx);
        persons.read(cx).get(&self.public_key, cx)
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
}

impl Render for ProfileDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let profile = self.profile(cx);
        let metadata = profile.metadata();

        // Get the public key as short string
        let shorten_pkey = SharedString::from(shorten_pubkey(self.public_key, 8));

        let about = metadata
            .about
            .filter(|about| !about.trim().is_empty())
            .map(SharedString::from);

        let website = metadata
            .website
            .filter(|website| !website.trim().is_empty())
            .map(SharedString::from);

        v_flex()
            .gap_3()
            .w_full()
            .child(
                v_flex()
                    .h_40()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .child(
                        Avatar::new(profile.avatar())
                            .seed(profile.avatar_seed())
                            .large(),
                    ),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(label("Name", cx))
                    .child(div().text_sm().child(profile.name())),
            )
            .when_some(about, |this, about| {
                this.child(
                    v_flex()
                        .gap_1p5()
                        .child(label("About", cx))
                        .child(div().text_sm().child(about)),
                )
            })
            .when_some(website, |this, website| {
                this.child(
                    v_flex()
                        .gap_1p5()
                        .child(label("Website", cx))
                        .child(div().text_sm().child(website)),
                )
            })
            .child(
                v_flex().gap_1p5().child(label("Public Key", cx)).child(
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
    }
}

fn label(label: &'static str, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().text_muted)
        .child(label)
}
