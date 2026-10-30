// The microphone's AudioWorklet processor: the one piece of the client that
// stays JavaScript, in any stack.
//
// A worklet runs in its own global scope — no DOM, no `WebAssembly.instantiate`
// of the app module, and `registerProcessor` must be called from JS there. So
// "the whole client in Rust" has a floor of about thirty lines of JS, and this is
// it (docs/roadmap.md §4.1). It is loaded by name (`audioWorklet.addModule
//('/capture.js')`) instead of a Blob URL built from a string inside the client —
// which is how `web/scripts/voice.ts` does it today.
//
// Batches 128-frame quanta into ~43 ms messages: 1024 samples at 24 kHz.
class Capture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.frame = new Int16Array(1024);
    this.filled = 0;
  }

  process(inputs) {
    const input = inputs[0];
    const channel = input ? input[0] : null;
    if (!channel) {
      return true;
    }
    for (let i = 0; i < channel.length; i += 1) {
      // Clamped before scaling, because the contract is signed 16-bit and a hot
      // input would otherwise wrap around into garbage.
      const sample = Math.max(-1, Math.min(1, channel[i]));
      this.frame[this.filled] = sample < 0 ? sample * 0x8000 : sample * 0x7fff;
      this.filled += 1;
      if (this.filled === this.frame.length) {
        // Transferred, not copied: the buffer is 2 KB per message at real-time
        // rate and this runs on the audio thread.
        this.port.postMessage(this.frame.buffer, [this.frame.buffer]);
        this.frame = new Int16Array(1024);
        this.filled = 0;
      }
    }
    return true;
  }
}

registerProcessor('capture', Capture);
