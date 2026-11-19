// The external half of the WASM bootstrap. Kept as a file (not an inline module)
// so the page's CSP can use `script-src 'self'`; the entry point and the legacy
// bundle name travel on this script tag's data attributes.
const script = document.querySelector('script[src="/assets/wasm-loader.js"]');
const entry = script?.dataset.entry ?? "initChat";
const legacy = script?.dataset.legacy ?? "旧客户端";

try {
  const client = await import("/wasm/kestrel_client.js");
  await client.default();
  if (entry === "initVoice") {
    client.initVoice();
  } else {
    client.initChat();
  }
} catch (error) {
  console.error("kestrel: the WASM client did not start", error);
  const note = document.createElement("p");
  note.className = "client-error";
  note.textContent = `WASM 客户端加载失败（${legacy} 是这个页面的旧客户端）。刷新页面重试。`;
  const stream = document.querySelector(".stream");
  (stream === null ? document.body : stream).append(note);
}
