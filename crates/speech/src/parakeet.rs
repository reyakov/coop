use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use gpui::{App, Task};
use gpui_kit::component::speech::{
    AudioFormat, RecognitionSession, SpeechError, SpeechRecognizer, SpeechSink,
};
use parakeet_rs::{ParakeetTDT, Transcriber as _};

const MODEL_RATE: u32 = 16_000;
const MAX_SECONDS: usize = 120;

/// The on-device recognizer.
pub struct ParakeetRecognizer;

impl SpeechRecognizer for ParakeetRecognizer {
    fn audio_format(&self) -> AudioFormat {
        AudioFormat::new(MODEL_RATE, 1)
    }

    fn is_available(&self, _cx: &App) -> bool {
        true
    }

    fn start(
        &self,
        sink: SpeechSink,
        cx: &mut App,
    ) -> Result<Box<dyn RecognitionSession>, SpeechError> {
        if super::model::installed() {
            send(Command::Warm(super::model::dir())).map_err(SpeechError::recognizer)?;
        } else if super::model::status().is_downloading() {
            return Err(SpeechError::recognizer(anyhow!(
                "The speech model is still downloading. Try again when it finishes."
            )));
        } else {
            return Err(SpeechError::recognizer(anyhow!(
                "No speech model is installed. Download it from Settings."
            )));
        }

        // The session buffers everything it is pushed,
        // so audio is accepted right away.
        sink.ready(cx);

        Ok(Box::new(ParakeetSession {
            sink,
            samples: Vec::new(),
            _result: None,
        }))
    }
}

struct ParakeetSession {
    sink: SpeechSink,
    samples: Vec<f32>,
    _result: Option<Task<()>>,
}

impl RecognitionSession for ParakeetSession {
    fn push_audio(&mut self, samples: &[i16], _cx: &mut App) {
        if self.samples.len() < MAX_SECONDS * MODEL_RATE as usize {
            self.samples
                .extend(samples.iter().map(|sample| f32::from(*sample) / 32_768.0));
        }
    }

    fn finish(&mut self, cx: &mut App) {
        let samples = std::mem::take(&mut self.samples);
        let sink = self.sink.clone();

        // Speech too short to transcribe ends the session at once.
        let silent = samples.iter().all(|sample| sample.abs() < 0.0001);
        if samples.len() < MODEL_RATE as usize / 5 || silent {
            sink.finish(cx);
            return;
        }

        let (sender, receiver) = flume::unbounded();

        if let Err(error) = send(Command::Transcribe { samples, sender }) {
            sink.error(SpeechError::recognizer(error), cx);
            return;
        }

        self._result = Some(cx.spawn(async move |cx| {
            let result = receiver.recv_async().await;

            cx.update(|cx| match result {
                Ok(Ok(text)) if !text.is_empty() => {
                    sink.phrase(text, cx);
                    sink.finish(cx);
                }
                Ok(Ok(_)) => sink.finish(cx),
                Ok(Err(error)) => sink.error(SpeechError::recognizer(error), cx),
                Err(_) => sink.error(
                    SpeechError::recognizer(anyhow!("The speech worker is unavailable")),
                    cx,
                ),
            });
        }));
    }
}

enum Command {
    Warm(PathBuf),
    Transcribe {
        samples: Vec<f32>,
        sender: flume::Sender<Result<String, anyhow::Error>>,
    },
    Unload,
}

fn worker_slot() -> &'static Mutex<Option<flume::Sender<Command>>> {
    static WORKER: OnceLock<Mutex<Option<flume::Sender<Command>>>> = OnceLock::new();
    WORKER.get_or_init(|| Mutex::new(None))
}

/// A handle to the live worker, spawning a replacement after an idle exit.
fn worker() -> Result<flume::Sender<Command>> {
    let mut slot = worker_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    if slot.as_ref().is_some_and(|sender| sender.is_disconnected()) {
        *slot = None;
    }

    if let Some(sender) = slot.as_ref() {
        return Ok(sender.clone());
    }

    let (sender, receiver) = flume::unbounded::<Command>();

    match std::thread::Builder::new()
        .name("speech-parakeet".into())
        .spawn(move || run_worker(receiver))
    {
        Ok(_) => {
            *slot = Some(sender.clone());
            Ok(sender)
        }
        Err(error) => {
            log::error!("failed to spawn the speech worker: {error}");
            Err(anyhow!("Could not start the speech worker: {error}"))
        }
    }
}

/// Deliver a command, retrying once on a fresh worker if the previous one exited.
fn send(command: Command) -> Result<()> {
    let command = match worker()?.send(command) {
        Ok(()) => return Ok(()),
        Err(error) => error.into_inner(),
    };
    // The worker can exit between the liveness check and the send, so retry once.
    worker()?.send(command).context("the speech worker exited")
}

fn run_worker(receiver: flume::Receiver<Command>) {
    let mut cached: Option<(PathBuf, ParakeetTDT)> = None;

    while let Ok(command) = receiver.recv_timeout(Duration::from_secs(30)) {
        match command {
            Command::Unload => {
                cached = None;
            }
            Command::Warm(dir) => {
                let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    load(&mut cached, &dir).map(|_| ())
                }));

                match flatten_panic(loaded) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) | Err(error) => {
                        log::warn!("failed to load the speech model: {error:#}")
                    }
                }
            }
            Command::Transcribe { samples, sender } => {
                let transcribed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    transcribe(&mut cached, samples)
                }));

                let result = match flatten_panic(transcribed) {
                    Ok(result) => result,
                    Err(_) => {
                        cached = None;
                        Err(anyhow!("Dictation stopped unexpectedly. Try again."))
                    }
                };

                if sender.send(result).is_err() {
                    log::debug!("speech session was cancelled before its result arrived");
                }
            }
        }
    }
}

fn load<'a>(
    cached: &'a mut Option<(PathBuf, ParakeetTDT)>,
    dir: &PathBuf,
) -> Result<&'a mut ParakeetTDT> {
    if cached.as_ref().is_none_or(|(path, _)| path != dir) {
        let model = ParakeetTDT::from_pretrained(dir, None)
            .map_err(|error| anyhow!("Could not load the speech model: {error}"))?;
        *cached = Some((dir.clone(), model));
    }

    let (_, model) = cached.as_mut().context("speech model was unloaded")?;
    Ok(model)
}

fn transcribe(cached: &mut Option<(PathBuf, ParakeetTDT)>, samples: Vec<f32>) -> Result<String> {
    let model = load(cached, &super::model::dir())?;

    model
        .transcribe_samples(samples, MODEL_RATE, 1, None)
        .map(|result| result.text)
        .map_err(|error| anyhow!("Could not transcribe this recording: {error}"))
}

/// Retire cached weights, e.g. after the model was deleted.
pub(crate) fn unload() {
    if let Err(error) = send(Command::Unload) {
        log::warn!("failed to unload the speech model: {error}");
    }
}

fn flatten_panic<T>(result: std::thread::Result<T>) -> Result<T> {
    result.map_err(|_| anyhow!("The speech worker panicked"))
}
