//! Browser file interoperability: a persistent hidden `<input type="file">`
//! picker, blob URLs for rendering, and anchor-based downloads.

use std::cell::RefCell;

use anyhow::{Error, anyhow};
use flume::Sender;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{File, HtmlAnchorElement, HtmlInputElement, Url as DomUrl, window as js_window};

use crate::file::PickedFile;

type PickResult = Result<Option<PickedFile>, Error>;

thread_local! {
    /// The picker input lives for the whole page so its change closure does
    /// not have to be leaked or re-registered on every pick
    static PICKER: RefCell<Option<Picker>> = const { RefCell::new(None) };
    /// The sender the active pick delivers through
    static PENDING: RefCell<Option<Sender<PickResult>>> = const { RefCell::new(None) };
}

struct Picker {
    input: HtmlInputElement,
    /// Keep the change handler alive for the life of the page
    _on_change: Closure<dyn FnMut()>,
}

/// Ask the user for a single file and read its bytes
pub async fn pick_file() -> PickResult {
    let (sender, receiver) = flume::bounded(1);

    PICKER.with(|picker| -> Result<(), Error> {
        let mut picker = picker.borrow_mut();
        if picker.is_none() {
            *picker = Some(init_picker()?);
        }
        let picker = picker.as_ref().expect("picker just initialized");

        PENDING.with(|pending| *pending.borrow_mut() = Some(sender));
        picker.input.click();

        Ok(())
    })?;

    receiver
        .recv_async()
        .await
        .map_err(|_| anyhow!("File picker closed"))?
}

fn init_picker() -> Result<Picker, Error> {
    let document = js_window()
        .ok_or_else(|| anyhow!("no browser window"))?
        .document()
        .ok_or_else(|| anyhow!("no browser document"))?;

    let input = document
        .create_element("input")
        .map_err(|e| anyhow!("failed to create file input: {e:?}"))?
        .dyn_into::<HtmlInputElement>()
        .map_err(|_| anyhow!("created element is not an input"))?;

    input.set_type("file");
    input.style().set_property("display", "none").ok();

    let on_change = Closure::wrap(Box::new(move || {
        let Some(sender) = PENDING.with(|pending| pending.borrow_mut().take()) else {
            return;
        };

        wasm_bindgen_futures::spawn_local(async move {
            sender.send_async(read_selected_file().await).await.ok();
        });
    }) as Box<dyn FnMut()>);

    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));

    let body = document.body().ok_or_else(|| anyhow!("no browser body"))?;
    body.append_child(&input)
        .map_err(|e| anyhow!("failed to mount file input: {e:?}"))?;

    Ok(Picker {
        input,
        _on_change: on_change,
    })
}

/// Read the first file of the picker input, if one was chosen
async fn read_selected_file() -> PickResult {
    let picker = PICKER
        .with(|picker| picker.borrow().as_ref().map(|picker| picker.input.clone()))
        .ok_or_else(|| anyhow!("file picker is not initialized"))?;

    let Some(file) = picker.files().and_then(|files| files.get(0)) else {
        // The dialog was dismissed without choosing a file
        return Ok(None);
    };

    let mime = non_empty(file.type_());
    let data = read_file_bytes(&file).await?;
    let local = crate::file::LocalFile::Url(blob_url(&mime, &data)?);

    Ok(Some(PickedFile {
        name: file.name(),
        mime,
        data,
        local,
    }))
}

/// Read a browser `File` into memory
async fn read_file_bytes(file: &File) -> Result<Vec<u8>, Error> {
    let promise = file.array_buffer();
    let buffer = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| anyhow!("failed to read file: {e:?}"))?;

    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// Create an object URL for bytes, so images and downloads render from memory
pub fn blob_url(mime: &str, data: &[u8]) -> Result<String, Error> {
    let array = js_sys::Uint8Array::from(data);
    let parts = js_sys::Array::new();
    parts.push(&array);

    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type(mime);

    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts.into(), &bag)
        .map_err(|e| anyhow!("failed to create blob: {e:?}"))?;

    DomUrl::create_object_url_with_blob(&blob).map_err(|e| anyhow!("failed to create url: {e:?}"))
}

/// Offer bytes to the user as a browser download
pub fn download(name: &str, mime: &str, data: &[u8]) -> Result<(), Error> {
    let window = js_window().ok_or_else(|| anyhow!("no browser window"))?;
    let document = window
        .document()
        .ok_or_else(|| anyhow!("no browser document"))?;

    let url = blob_url(mime, data)?;

    let anchor: HtmlAnchorElement = document
        .create_element("a")
        .map_err(|e| anyhow!("failed to create download link: {e:?}"))?
        .dyn_into()
        .map_err(|_| anyhow!("created element is not an anchor"))?;
    anchor.set_href(&url);
    anchor.set_download(name);
    anchor.style().set_property("display", "none").ok();

    let body = document.body().ok_or_else(|| anyhow!("no browser body"))?;
    body.append_child(&anchor)
        .map_err(|e| anyhow!("failed to mount download link: {e:?}"))?;
    anchor.click();
    body.remove_child(&anchor)
        .map_err(|e| anyhow!("failed to remove download link: {e:?}"))?;

    // Keep the blob alive briefly so the download is not cancelled
    let callback = Closure::wrap(Box::new(move || {
        DomUrl::revoke_object_url(&url).ok();
    }) as Box<dyn FnMut()>);
    window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            10_000,
        )
        .map_err(|e| anyhow!("failed to schedule cleanup: {e:?}"))?;
    // The closure only runs once, after which it can be released
    callback.forget();

    Ok(())
}

/// Open a URL in a new browser tab
pub fn open_url(url: &str) -> Result<(), Error> {
    js_window()
        .ok_or_else(|| anyhow!("no browser window"))?
        .open_with_url_and_target(url, "_blank")
        .map_err(|e| anyhow!("failed to open url: {e:?}"))?
        .ok_or_else(|| anyhow!("the browser blocked opening the url"))?;

    Ok(())
}

fn non_empty(value: impl Into<String>) -> String {
    let value: String = value.into();

    if value.is_empty() {
        "application/octet-stream".to_string()
    } else {
        value
    }
}
