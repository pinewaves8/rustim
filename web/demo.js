// The demo page's glue: load the module, build one host, wire the command box.
//
// Everything else is Rust. This file exists so `index.html` has no logic worth
// reviewing, and so the acceptance script has a stable handle (`window.rustim`).

// The formula page is a *second artifact* — `pkg-formula` carries Typst and is
// roughly an order of magnitude larger — so the page says which module to load
// rather than a URL parameter deciding it at run time.
const pkg = document.body.dataset.pkg || 'pkg';
const { default: init, WebApp } = await import(`./${pkg}/rustim_web.js`);

const params = new URLSearchParams(location.search);
// `?paused=1` is the acceptance script's mode: draw virtual time zero, then
// call `advance(10)` and screenshot the result. A human opening the page sees
// the animation play instead.
const paused = params.get('paused') === '1';
const scene = params.get('scene') || document.body.dataset.scene || 'pythagorean';

await init();
const app = new WebApp('canvas', scene, !paused);
window.rustim = app;

const form = document.getElementById('command');
const input = document.getElementById('line');
const reply = document.getElementById('reply');

form.addEventListener('submit', (event) => {
  event.preventDefault();
  const answer = app.apply(input.value);
  // `undefined` is a blank line: nothing was asked, so nothing is answered.
  if (answer !== undefined) {
    reply.textContent = answer;
  }
  input.value = '';
});

const failure = app.render_error();
if (failure !== undefined) {
  reply.textContent = `渲染失败：${failure}`;
}

// ---------------------------------------------------------------------------
// Recording the canvas to a file, in real time.
//
// **This is the page's feature and not the engine's export path**, and the
// distinction is the whole design. `VideoSink` — the engine's seam — writes one
// frame at a time with the *virtual* clock's timestamps, which is what makes an
// export reproducible frame for frame. `MediaRecorder` records whatever the
// canvas happens to be showing while wall-clock time passes: the same scene
// recorded twice gives two different files, and a backgrounded tab records
// nothing at all.
//
// A browser `VideoSink` is the other road, and it is not taken: `VideoEncoder`
// exists on a secure origin (`127.0.0.1` is one; `data:` and `file:` are not —
// measured, `typeof VideoEncoder === "undefined"` there), but encoded chunks are
// not a playable file, so it needs a muxer as well. That is a third-party
// dependency this page deliberately does not carry, and the honest place for it
// is a decision rather than an import.
//
// The codecs are tried in order because a headless build does not promise all of
// them; `MediaRecorder` throws on a `mimeType` it cannot produce.

/// The first container/codec this browser will actually record.
function recordingType() {
  for (const type of [
    'video/webm;codecs=vp9',
    'video/webm;codecs=vp8',
    'video/webm',
    'video/mp4',
  ]) {
    if (typeof MediaRecorder !== 'undefined' && MediaRecorder.isTypeSupported(type)) {
      return type;
    }
  }
  return null;
}

/// Record `seconds` of the canvas and resolve with what came out.
///
/// The resolved object's `bytes` is what the acceptance script asserts on: a
/// recording that produced no data is a failure that otherwise looks like a
/// successful download of nothing.
async function record(seconds = 5, fps = 30) {
  const type = recordingType();
  if (type === null) {
    return { error: 'this browser has no MediaRecorder codec' };
  }
  const canvas = document.getElementById('canvas');
  const stream = canvas.captureStream(fps);
  const chunks = [];
  const recorder = new MediaRecorder(stream, { mimeType: type });
  recorder.ondataavailable = (event) => {
    if (event.data && event.data.size > 0) {
      chunks.push(event.data);
    }
  };
  const stopped = new Promise((resolve) => {
    recorder.onstop = resolve;
  });
  recorder.start();
  await new Promise((resolve) => setTimeout(resolve, seconds * 1000));
  recorder.stop();
  await stopped;
  const blob = new Blob(chunks, { type });
  return { bytes: blob.size, type, chunks: chunks.length, url: URL.createObjectURL(blob) };
}

window.rustimRecord = record;
