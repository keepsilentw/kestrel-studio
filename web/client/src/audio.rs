//! The audio graph: microphone in, server audio out.
//!
//! This is the half of the client that cannot be tested on this machine at all:
//! `getUserMedia` needs a permission prompt and a real device, so "the real
//! microphone has never been verified" stays true from the Nest version through
//! this one (docs/roadmap.md §3) until someone presses the button on a deployment.
//! What *can* be tested lives in [`crate::pcm`] — base64, PCM16 endianness,
//! clamping, the odd trailing byte — and is asserted on the host target.
//!
//! The capture processor itself is JavaScript, in `web/public/capture.js`: an
//! `AudioWorkletProcessor` runs in the worklet global scope, which has no DOM and
//! no host for an application WASM module, so `registerProcessor` can only be
//! called from JS. That file is the floor of "no JS in the repository"
//! (docs/roadmap.md §4.1).
//!
//! Three properties this module is written to keep:
//!
//! * **stereo never reaches the wire.** An `AudioWorkletNode` defaults to stereo;
//!   the upstream contract is mono PCM16 at 24 kHz and nothing rejects the wrong
//!   shape — it just sounds wrong. `channel_count: 1` with an *explicit* mode is
//!   load-bearing, as is `numberOfOutputs: 0`, which keeps the microphone out of
//!   the speakers (no feedback path).
//! * **playback is queued, not started on arrival.** Each chunk is scheduled at
//!   `play_head`, which then advances by that buffer's duration, so consecutive
//!   chunks are contiguous instead of overlapping or leaving gaps.
//! * **no borrow is held across an `await`.** Every step below borrows the shared
//!   graph for one statement. A future that kept a `RefCell` borrow alive over
//!   `getUserMedia` would panic the moment the user released the button while the
//!   prompt was still up — which is exactly when they will do it.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AudioContext, AudioContextOptions, AudioWorkletNode, AudioWorkletNodeOptions,
    ChannelCountMode, MediaStream, MediaStreamAudioSourceNode, MediaStreamConstraints,
    MediaStreamTrack, MessageEvent, MessagePort,
};

/// The upstream's sample rate. Asking the browser for it is what lets the browser
/// resample the microphone for us — the reason there is no resampler here.
pub const SAMPLE_RATE: f32 = 24_000.0;

/// A live microphone. The stream is kept because `disconnect()` alone is not
/// teardown: the track has to stop or the browser keeps the recording indicator on.
struct Mic {
    stream: MediaStream,
    source: MediaStreamAudioSourceNode,
    node: AudioWorkletNode,
}

pub struct AudioGraph {
    context: Option<AudioContext>,
    mic: Option<Mic>,
    play_head: f64,
}

impl AudioGraph {
    pub fn new() -> Self {
        AudioGraph { context: None, mic: None, play_head: 0.0 }
    }

    pub fn has_context(&self) -> bool {
        self.context.is_some()
    }

    pub fn is_capturing(&self) -> bool {
        self.mic.is_some()
    }

    /// Create the context if there is none. Synchronous on purpose: the only
    /// awaiting this module does happens in [`open_mic`], outside any borrow.
    ///
    /// The context is created on the first press, never earlier — both
    /// `getUserMedia` and a resumed context require a user gesture, and opening a
    /// microphone on a page that is merely being looked at is not acceptable.
    fn ensure_context(&mut self) -> Result<AudioContext, String> {
        if let Some(context) = &self.context {
            return Ok(context.clone());
        }
        let options = AudioContextOptions::new();
        options.set_sample_rate(SAMPLE_RATE);
        let context = AudioContext::new_with_context_options(&options)
            .map_err(|error| format!("cannot create the audio context: {error:?}"))?;
        self.context = Some(context.clone());
        Ok(context)
    }

    fn attach(&mut self, stream: MediaStream, source: MediaStreamAudioSourceNode, node: AudioWorkletNode) {
        self.mic = Some(Mic { stream, source, node });
    }

    /// Schedule one chunk of the answer at the head of the queue.
    pub fn play_pcm(&mut self, bytes: &[u8]) -> Result<(), String> {
        let context = self
            .context
            .as_ref()
            // Loud rather than silent: the Nest client returned early here when
            // there was no context, which is how a missing frame becomes an
            // unexplained gap in the answer.
            .ok_or_else(|| "audio arrived before the audio graph existed")?
            .clone();

        let samples = crate::pcm::pcm16_le_to_f32(bytes);
        if samples.is_empty() {
            return Ok(());
        }
        let buffer = context
            .create_buffer(1, samples.len() as u32, SAMPLE_RATE)
            .map_err(|error| format!("cannot create the playback buffer: {error:?}"))?;
        let channel = js_sys::Float32Array::new_with_length(samples.len() as u32);
        for (index, sample) in samples.iter().enumerate() {
            channel.set_index(index as u32, *sample);
        }
        buffer
            .copy_to_channel_with_f32_array(&channel, 0)
            .map_err(|error| format!("cannot fill the playback buffer: {error:?}"))?;

        let source = context
            .create_buffer_source()
            .map_err(|error| format!("cannot create a buffer source: {error:?}"))?;
        source.set_buffer(Some(&buffer));
        source
            .connect_with_audio_node(&context.destination())
            .map_err(|error| format!("cannot connect playback: {error:?}"))?;

        self.play_head = self.play_head.max(context.current_time());
        source
            .start_with_when(self.play_head)
            .map_err(|error| format!("cannot start playback: {error:?}"))?;
        self.play_head += buffer.duration();
        Ok(())
    }

    /// Tear the microphone down, leaving the context reusable.
    pub fn stop_capture(&mut self) {
        if let Some(mic) = self.mic.take() {
            let _ = mic.node.disconnect();
            let _ = mic.source.disconnect();
            for value in mic.stream.get_tracks() {
                if let Ok(track) = value.dyn_into::<MediaStreamTrack>() {
                    track.stop();
                }
            }
        }
    }
}

impl Default for AudioGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// Load the worklet, open the microphone, and forward every 1024-sample frame to
/// `on_frame` as raw PCM16 little-endian bytes (the base64 wrapper is `pcm`'s
/// job, and it is tested).
///
/// `graph` is shared rather than borrowed because the steps below await; the only
/// borrows are the two single statements that read the context and store the mic.
pub async fn open_mic(
    graph: Rc<RefCell<AudioGraph>>,
    on_frame: Rc<dyn Fn(Vec<u8>)>,
) -> Result<(), String> {
    let context = { graph.borrow_mut().ensure_context()? };

    JsFuture::from(context.resume().map_err(|error| format!("cannot resume audio: {error:?}"))?)
        .await
        .map_err(|error| format!("cannot resume audio: {error:?}"))?;

    // A context that reports a different rate means the browser ignored the
    // request; say so on the page, because the alternative reading is that the
    // model's voice is oddly pitched.
    let actual = context.sample_rate();
    if (actual - SAMPLE_RATE).abs() > 0.5 {
        web_sys::console::warn_2(
            &JsValue::from_str("kestrel voice: the browser changed the sample rate to"),
            &JsValue::from(actual),
        );
    }

    let worklet = context
        .audio_worklet()
        .map_err(|error| format!("this browser has no audioWorklet: {error:?}"))?;
    JsFuture::from(
        worklet
            // Served as a static file rather than assembled from a string at
            // runtime: the worklet source is the JS floor, and a file is
            // reviewable while a Blob URL is not.
            .add_module("/capture.js")
            .map_err(|error| format!("cannot load the capture worklet: {error:?}"))?,
    )
    .await
    .map_err(|error| format!("the capture worklet failed to load: {error:?}"))?;

    let window = web_sys::window().ok_or("no window")?;
    let devices = window
        .navigator()
        .media_devices()
        .map_err(|_| "拿不到麦克风权限，无法说话".to_string())?;
    let constraints = MediaStreamConstraints::new();
    constraints.set_audio_bool(true);
    let permission = JsFuture::from(
        devices
            .get_user_media_with_constraints(&constraints)
            .map_err(|error| format!("getUserMedia failed: {error:?}"))?,
    )
    .await
    .map_err(|_| "拿不到麦克风权限，无法说话".to_string())?;
    let stream: MediaStream = permission.dyn_into().map_err(|_| "getUserMedia returned garbage")?;

    let source = context
        .create_media_stream_source(&stream)
        .map_err(|error| format!("cannot read the microphone: {error:?}"))?;

    let options = AudioWorkletNodeOptions::new();
    options.set_number_of_inputs(1);
    options.set_number_of_outputs(0);
    options.set_channel_count(1);
    options.set_channel_count_mode(ChannelCountMode::Explicit);
    let node = AudioWorkletNode::new_with_options(&context, "capture", &options)
        .map_err(|error| format!("cannot create the capture node: {error:?}"))?;
    source
        .connect_with_audio_node(&node)
        .map_err(|error| format!("cannot connect the microphone: {error:?}"))?;

    let port: MessagePort = node.port().map_err(|error| format!("the worklet has no port: {error:?}"))?;
    let forward = Rc::clone(&on_frame);
    let listener = Closure::wrap(Box::new(move |event: MessageEvent| {
        // The worklet transfers its buffer, so the payload is an ArrayBuffer.
        forward(js_sys::Uint8Array::new(&event.data()).to_vec());
    }) as Box<dyn Fn(MessageEvent)>);
    port.add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
        .map_err(|error| format!("cannot listen to the worklet: {error:?}"))?;
    listener.forget();

    graph.borrow_mut().attach(stream, source, node);
    Ok(())
}

/// Fire [`open_mic`] and hand the outcome back to the page. A failure has to
/// reach the page as a message: a button that records nothing looks identical to
/// one that is merely waiting for speech.
pub fn spawn_open_mic(
    graph: Rc<RefCell<AudioGraph>>,
    on_frame: Rc<dyn Fn(Vec<u8>)>,
    on_result: Rc<dyn Fn(Result<(), String>)>,
) {
    spawn_local(async move {
        let outcome = open_mic(graph, on_frame).await;
        on_result(outcome);
    });
}
