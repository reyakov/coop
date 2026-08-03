# browser-signer-proxy

Proxy to use Nostr Browser signer ([NIP-07](https://github.com/nostr-protocol/nips/blob/master/07.md)) in native applications.

This is a re-implementation of [`nostr-browser-signer-proxy`](https://github.com/nostrdevkit/nostr/tree/master/signer/nostr-browser-signer-proxy)
using the [`smol`](https://github.com/smol-rs/smol) async runtime instead of tokio.

## Description

This crate provides a local HTTP proxy that communicates with a NIP-07 browser extension
(e.g., Alby, nos2x) running in a browser tab. Native applications can use this proxy to
request public keys, sign events, and perform NIP-04/NIP-44 encryption/decryption through
the browser extension.

The HTTP server is implemented with a minimal, dependency-free approach using `smol::net::TcpListener`
and manual HTTP/1.1 parsing — avoiding heavy HTTP framework dependencies entirely.

## Usage

```rust
use browser_signer_proxy::prelude::*;

async fn example() -> Result<(), Error> {
    // Create the proxy with default options (localhost:7400)
    let proxy = BrowserSignerProxy::new(BrowserSignerProxyOptions::default());

    // Open the proxy URL in a browser
    webbrowser::open(&proxy.url())?;

    // Start the proxy server
    proxy.start().await?;

    // Use it as an async Nostr signer
    let public_key = proxy.get_public_key_async().await?;
    println!("Connected with public key: {public_key}");

    Ok(())
}
```

## Differences from the tokio-based version

| Feature | tokio (original) | smol (this crate) |
|---|---|---|
| Async runtime | `tokio` | `smol` |
| HTTP server | `hyper` | `smol::net::TcpListener` + manual HTTP/1.1 |
| Mutex | `tokio::sync::Mutex` | `smol::lock::Mutex` |
| Shutdown signal | `tokio::sync::Notify` | `event_listener::Event` |
| Request-response channel | `tokio::sync::oneshot` | `smol::channel::bounded(1)` |
| Timeout | `tokio::time::timeout` | `smol::future::or` + `smol::Timer` |
| Task spawning | `tokio::spawn` | `smol::spawn` |

## License

This project is distributed under the MIT software license.
