use std::time::Duration;

use tts::Tts;

/// Words per minute of an engine at its normal rate, for estimating speech duration.
const NORMAL_WPM: f32 = 160.0;

/// Extra time past the estimate before speech is considered finished, for backends that
/// cannot report utterance end.
pub const END_GRACE: Duration = Duration::from_secs(5);

/// Reads text aloud through the platform's speech backend, created on first use.
pub struct Speaker {
    tts: Option<Tts>,
    end_tx: flume::Sender<()>,
    end_rx: flume::Receiver<()>,
}

impl Speaker {
    pub fn new() -> Self {
        let (end_tx, end_rx) = flume::unbounded();

        Self {
            tts: None,
            end_tx,
            end_rx,
        }
    }

    /// Cloneable stream of utterance-finish events.
    pub fn end_events(&self) -> flume::Receiver<()> {
        self.end_rx.clone()
    }

    /// Speak the text, interrupting what this speaker is already saying.
    pub fn speak(&mut self, text: &str) -> Result<Duration, tts::Error> {
        let end_tx = self.end_tx.clone();
        let tts = match self.tts.as_ref() {
            Some(tts) => tts,
            None => self.tts.insert({
                let tts = Tts::default()?;
                if let Err(error) = tts.on_utterance_end(move |_| {
                    // A send error only means the panel dropped its event loop.
                    let _ = end_tx.send(());
                }) {
                    log::warn!("utterance end events unavailable ({error})");
                }
                tts
            }),
        };
        tts.speak(text, true)?;
        Ok(Self::estimate(tts, text))
    }

    /// Stop speaking, a speaker that never spoke is already silent.
    pub fn stop(&mut self) -> Result<(), tts::Error> {
        match self.tts.as_ref() {
            Some(tts) => tts.stop()?,
            None => return Ok(()),
        }

        Ok(())
    }

    /// Best-effort speaking state, a speaker that never spoke is silent.
    pub fn speaking(&self) -> bool {
        self.tts
            .as_ref()
            .and_then(|tts| tts.is_speaking().ok())
            .unwrap_or(false)
    }

    /// How long speaking the text is expected to take at the engine's current speed.
    fn estimate(tts: &Tts, text: &str) -> Duration {
        let words = text.split_whitespace().count().max(1) as f32;
        let speed = match (tts.get_rate(), tts.normal_rate()) {
            (Ok(rate), Ok(normal)) if normal > 0. => (rate / normal).clamp(0.2, 4.),
            _ => 1.,
        };
        Duration::from_secs_f32(words / NORMAL_WPM * 60. / speed)
    }
}

impl Default for Speaker {
    fn default() -> Self {
        Self::new()
    }
}
