// The demo page's glue: load the module, build one host, wire the command box.
//
// Everything else is Rust. This file exists so `index.html` has no logic worth
// reviewing, and so the acceptance script has a stable handle (`window.rustim`).

import init, { WebApp } from './pkg/rustim_web.js';

const params = new URLSearchParams(location.search);
// `?paused=1` is the acceptance script's mode: draw virtual time zero, then
// call `advance(10)` and screenshot the result. A human opening the page sees
// the animation play instead.
const paused = params.get('paused') === '1';
const scene = params.get('scene') || 'pythagorean';

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
