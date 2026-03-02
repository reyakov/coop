use std::sync::Arc;
use std::time::Duration;

use common::TextUtils;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, img, px, AppContext, Context, Entity, Image, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window,
};
use nostr_connect::prelude::*;
use state::{
    CoopAuthUrlHandler, NostrRegistry, SignerEvent, CLIENT_NAME, NOSTR_CONNECT_RELAY,
    NOSTR_CONNECT_TIMEOUT,
};
use theme::ActiveTheme;
use ui::v_flex;

pub struct ConnectSigner {
    /// QR Code
    qr_code: Option<Arc<Image>>,

    /// Error message
    error: Entity<Option<SharedString>>,

    /// Subscription to the signer event
    _subscription: Option<Subscription>,
}

impl ConnectSigner {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let error = cx.new(|_| None);

        let nostr = NostrRegistry::global(cx);
        let app_keys = nostr.read(cx).app_keys.clone();

        let timeout = Duration::from_secs(NOSTR_CONNECT_TIMEOUT);
        let relay = RelayUrl::parse(NOSTR_CONNECT_RELAY).unwrap();

        // Generate the nostr connect uri
        let uri = NostrConnectUri::client(app_keys.public_key(), vec![relay], CLIENT_NAME);

        // Generate the nostr connect
        let mut signer = NostrConnect::new(uri.clone(), app_keys.clone(), timeout, None).unwrap();

        // Handle the auth request
        signer.auth_url_handler(CoopAuthUrlHandler);

        // Generate a QR code for quick connection
        let qr_code = uri.to_string().to_qr();

        // Set signer in the background
        nostr.update(cx, |this, cx| {
            this.add_nip46_signer(&signer, cx);
        });

        // Subscribe to the signer event
        let subscription = cx.subscribe_in(&nostr, window, |this, _state, event, _window, cx| {
            if let SignerEvent::Error(e) = event {
                this.set_error(e, cx);
            }
        });

        Self {
            qr_code,
            error,
            _subscription: Some(subscription),
        }
    }

    fn set_error<S>(&mut self, message: S, cx: &mut Context<Self>)
    where
        S: Into<SharedString>,
    {
        self.error.update(cx, |this, cx| {
            *this = Some(message.into());
            cx.notify();
        });
    }
}

impl Render for ConnectSigner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const MSG: &str = "Scan with any Nostr Connect-compatible app to connect";

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_4()
            .when_some(self.qr_code.as_ref(), |this, qr| {
                this.child(
                    img(qr.clone())
                        .size(px(256.))
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(cx.theme().border),
                )
            })
            .when_some(self.error.read(cx).as_ref(), |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(cx.theme().danger_active)
                        .child(error.clone()),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().text_muted)
                    .child(SharedString::from(MSG)),
            )
    }
}
