use std::cell::RefCell;

use anyhow::anyhow;
use gpui::App;
use gpui_kit::Subscription;
use gpui_kit::component::speech::{
    AudioFormat, AudioInput, AudioSink, RecognitionSession, SpeechError, SpeechRecognizer,
    SpeechSink,
};
use js_sys::{Array, Function, JsString, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use web_sys::window;

/// The recognizer backed by `SpeechRecognition` (or its `webkit` prefix).
pub struct WebSpeechRecognizer;

impl SpeechRecognizer for WebSpeechRecognizer {
    fn audio_format(&self) -> AudioFormat {
        AudioFormat::default()
    }

    fn is_available(&self, _cx: &App) -> bool {
        true
    }

    fn start(
        &self,
        sink: SpeechSink,
        cx: &mut App,
    ) -> Result<Box<dyn RecognitionSession>, SpeechError> {
        let recognition = Recognition::open(sink, cx).map_err(SpeechError::recognizer)?;

        Ok(Box::new(recognition))
    }
}

pub struct SelfCapturedInput;

impl AudioInput for SelfCapturedInput {
    fn start(
        &self,
        _format: AudioFormat,
        _sink: AudioSink,
        _cx: &mut App,
    ) -> Result<Subscription, SpeechError> {
        Ok(Subscription::new(|| {}))
    }
}

enum Event {
    Hypothesis(String),
    Phrase(String),
    Error(String),
    End,
}

struct Recognition {
    recognition: JsValue,
    on_result: Option<Closure<dyn FnMut(JsValue)>>,
    on_error: Option<Closure<dyn FnMut(JsValue)>>,
    on_end: Option<Closure<dyn FnMut(JsValue)>>,
    _task: gpui::Task<()>,
}

impl Recognition {
    fn open(sink: SpeechSink, cx: &mut App) -> anyhow::Result<Self> {
        let window = window().ok_or_else(|| anyhow!("No browser window"))?;

        let constructor = Reflect::get(&window, &JsString::from("SpeechRecognition"))
            .ok()
            .filter(|value| value.is_function())
            .or_else(|| {
                Reflect::get(&window, &JsString::from("webkitSpeechRecognition"))
                    .ok()
                    .filter(|value| value.is_function())
            })
            .ok_or_else(|| anyhow!("This browser does not support speech recognition"))?;

        let recognition =
            Reflect::construct(constructor.unchecked_ref::<Function>(), &Array::new())
                .map_err(|error| js_error("constructing the recognizer", error))?;

        let (sender, receiver) = flume::unbounded::<Event>();
        let error_sender = sender.clone();
        let end_sender = sender.clone();

        let finalized = RefCell::new(0usize);
        let on_result = Closure::wrap(Box::new(move |event: JsValue| {
            let read = |value: &JsValue, property: &str| -> Option<JsValue> {
                Reflect::get(value, &JsString::from(property)).ok()
            };

            let Some(results) = read(&event, "results") else {
                return;
            };
            let length = read(&results, "length")
                .and_then(|length| length.as_f64())
                .unwrap_or(0.0) as usize;

            let mut finalized = finalized.borrow_mut();
            let mut interim = String::new();

            for index in *finalized..length {
                let Some(result) = read(&results, &index.to_string()) else {
                    continue;
                };

                let Some(alternative) = read(&result, "0") else {
                    continue;
                };

                let Some(text) = read(&alternative, "transcript").and_then(|text| text.as_string())
                else {
                    continue;
                };

                let is_final = read(&result, "isFinal")
                    .and_then(|final_value| final_value.as_bool())
                    .unwrap_or(false);

                if is_final {
                    *finalized += 1;
                    if sender.send(Event::Phrase(text)).is_err() {
                        log::debug!("speech session was cancelled mid-utterance");
                    }
                } else {
                    interim.push_str(&text);
                }
            }

            if !interim.is_empty() && sender.send(Event::Hypothesis(interim)).is_err() {
                log::debug!("speech session was cancelled mid-utterance");
            }
        }) as Box<dyn FnMut(JsValue)>);

        let on_error = Closure::wrap(Box::new(move |event: JsValue| {
            let message = Reflect::get(&event, &JsString::from("error"))
                .ok()
                .and_then(|error| error.as_string())
                .unwrap_or_else(|| "unknown speech error".into());

            if error_sender.send(Event::Error(message)).is_err() {
                log::debug!("speech session was cancelled before its error arrived");
            }
        }) as Box<dyn FnMut(JsValue)>);

        let on_end = Closure::wrap(Box::new(move |_event: JsValue| {
            if end_sender.send(Event::End).is_err() {
                log::debug!("speech session was cancelled before its end arrived");
            }
        }) as Box<dyn FnMut(JsValue)>);

        set_callback(&recognition, "onresult", on_result.as_ref().unchecked_ref())?;
        set_callback(&recognition, "onerror", on_error.as_ref().unchecked_ref())?;
        set_callback(&recognition, "onend", on_end.as_ref().unchecked_ref())?;
        set_bool(&recognition, "continuous", true)?;
        set_bool(&recognition, "interimResults", true)?;

        call_method(&recognition, "start")?;

        // The browser consumes audio as soon as it has permission
        sink.ready(cx);

        let task = cx.spawn(async move |cx| {
            while let Ok(event) = receiver.recv_async().await {
                let done = matches!(event, Event::End);

                // The speech state ignores sink calls once its session ended,
                // so a late event after a cancel is harmless.
                cx.update(|cx| match event {
                    Event::Hypothesis(text) => sink.hypothesis(text, cx),
                    Event::Phrase(text) => sink.phrase(text, cx),
                    Event::Error(message) => {
                        sink.error(SpeechError::recognizer(anyhow!("{message}")), cx)
                    }
                    Event::End => sink.finish(cx),
                });

                if done {
                    break;
                }
            }
        });

        Ok(Self {
            recognition,
            on_result: Some(on_result),
            on_error: Some(on_error),
            on_end: Some(on_end),
            _task: task,
        })
    }

    fn stop(&mut self) {
        self.on_result = None;
        self.on_error = None;
        self.on_end = None;

        let _ignored = call_method(&self.recognition, "abort");
    }
}

impl Drop for Recognition {
    fn drop(&mut self) {
        self.stop();
    }
}

impl RecognitionSession for Recognition {
    fn push_audio(&mut self, _samples: &[i16], _cx: &mut App) {
        // The browser recognizer captures the microphone itself.
        // the state still pushes audio through its input, which is a no-op here.
    }

    fn finish(&mut self, _cx: &mut App) {
        // The end handler reports completion, so no result is fabricated here.
        let _ignored = call_method(&self.recognition, "stop");
    }
}

fn set_callback(target: &JsValue, property: &str, callback: &Function) -> anyhow::Result<()> {
    Reflect::set(target, &JsString::from(property), callback)
        .map_err(|error| js_error(property, error))?;
    Ok(())
}

fn set_bool(target: &JsValue, property: &str, value: bool) -> anyhow::Result<()> {
    Reflect::set(target, &JsString::from(property), &JsValue::from(value))
        .map_err(|error| js_error(property, error))?;
    Ok(())
}

fn call_method(target: &JsValue, method: &str) -> anyhow::Result<()> {
    let function = Reflect::get(target, &JsString::from(method))
        .map_err(|error| js_error(method, error))?
        .unchecked_into::<Function>();

    function
        .call0(target)
        .map(|_| ())
        .map_err(|error| js_error(method, error))
}

fn js_error(what: &str, error: JsValue) -> anyhow::Error {
    let description = error
        .as_string()
        .or_else(|| {
            Reflect::get(&error, &JsString::from("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| "unknown error".into());

    anyhow!("{what} failed: {description}")
}
