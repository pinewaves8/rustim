// The browser acceptance check: a real Chrome, a real canvas, real pixels.
//
// What it proves, and why each part is here rather than in `cargo test`:
//
//  1. the module loads and `WebApp::new` builds the same scene the desktop runs;
//  2. ten seconds of virtual time on the canvas equals the desktop `--png`
//     still, measured by the same `rustim-compare` the golden suite uses;
//  3. the rasteriser reported no error;
//  4. Chrome's accessibility tree holds the four named figures with their
//     measured descriptions — the overlay is `opacity: 0`, and this proves
//     Chrome did not prune it;
//  5. the command bridge answers an `add` with the id it created and refuses
//     broken JSON;
//  6. a pointer drag and a wheel event, dispatched through Chrome's own input
//     pipeline, move the camera the way the desktop's shared gesture code does:
//     200 CSS px of drag is 200 canvas px of pan, and a -100 px wheel is a
//     zoom factor of `e^0.2` about the point under the cursor.
//
// Measured on 2026-10-17 (Chrome 151, linux-aarch64): the canvas agrees with
// the binary's PNG to a maximum channel error of **1** on 16 pixels (0.002%).
// That is not colour management — it is the canvas's premultiplied backing
// store round-tripping the alpha-254 pixels where the translucent square meets
// the background. `putImageData` itself is exact; `toDataURL` has to
// un-premultiply, and Chrome's unpremultiply rounds one LSB away from ours on
// those pixels. The content bounding box is identical and the golden
// tolerance (§5.2) is met with room to spare; demanding `max == 0` here would
// be asserting a property the canvas API does not offer.
//
// Re-measured on 2026-10-18 (Chrome 155, macos-aarch64, real Retina display):
// **every number is the same** — content box `(343, 90)..(936, 629)`,
// 16 pixels at 1 LSB, max channel error 1, and the same 6.28e-16 anchor drift on
// the wheel. The `--force-device-scale-factor=1` flag below is what keeps a 2×
// display out of the comparison; without it the canvas would be 2560x1440 and
// the box would not match a 1280x720 still.
//
// `cargo test -p rustim-web` covers the session core natively; only the canvas
// round-trip needs a browser, and that is what this file adds.
//
// Zero npm dependencies on purpose: a check that needs an install is a check
// that rots. Node 24 ships `WebSocket`, and Chrome speaks the DevTools
// protocol over it directly.
//
//   node tools/web-check.mjs [--keep-open]
//
// Exit: 0 the frame matches, 1 it does not, 2 the question could not be asked.

import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { delimiter, extname, join, normalize, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const webRoot = join(root, 'web');
const outDir = join(root, 'target', 'web-artifacts');
const keepOpen = process.argv.includes('--keep-open');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// ---------------------------------------------------------------------------
// A static server for `web/`, because `file://` cannot fetch a .wasm module.

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm',
  '.png': 'image/png',
};

const server = createServer(async (request, response) => {
  const path = normalize(join(webRoot, decodeURIComponent(new URL(request.url, 'http://x').pathname)));
  if (!path.startsWith(webRoot + sep)) {
    response.writeHead(403).end();
    return;
  }
  try {
    const body = await readFile(path);
    response.writeHead(200, { 'content-type': MIME[extname(path)] || 'application/octet-stream' });
    response.end(body);
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

// ---------------------------------------------------------------------------
// Chrome, headless, with its own profile so it never touches the user's.

/// Where Chrome lives, tried in a fixed order: `$CHROME` first, then the names it
/// goes by on `PATH`, then the macOS bundles.
///
/// The default used to be the bare `google-chrome`, which is the *Linux* name —
/// so on macOS this check could never run, and said so with a confusing "chrome
/// exited before the check ran" rather than "no browser here". `$CHROME` still
/// wins, so a caller can point at a Chromium or a Chrome Beta.
function findChrome() {
  if (process.env.CHROME) return process.env.CHROME;

  const names = [
    'google-chrome',
    'google-chrome-stable',
    'chromium',
    'chromium-browser',
  ];
  for (const dir of (process.env.PATH || '').split(delimiter)) {
    if (!dir) continue;
    for (const name of names) {
      const candidate = join(dir, name);
      if (existsSync(candidate)) return candidate;
    }
  }

  // The bundle, not the bare name: macOS ships no `google-chrome` executable, and
  // a `.app` is a directory, so the binary is three levels inside it.
  const bundles = [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
  ];
  for (const candidate of bundles) {
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

const chromePath = findChrome();
if (chromePath === null) {
  console.error(
    'no Chrome found: set $CHROME, or install Chrome/Chromium.\n' +
      '  (exit 2 — the question could not be asked)',
  );
  process.exit(2);
}
const profile = join(outDir, 'chrome-profile');
await rm(profile, { recursive: true, force: true });
const chrome = spawn(
  chromePath,
  [
    '--headless=new',
    '--no-sandbox',
    '--disable-gpu',
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-extensions',
    '--hide-scrollbars',
    '--force-device-scale-factor=1',
    '--window-size=1300,900',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    `${origin}/index.html?paused=1`,
  ],
  { stdio: ['ignore', 'ignore', 'pipe'] },
);

let chromeLog = '';
const browserWs = await new Promise((resolve, reject) => {
  const timer = setTimeout(() => {
    reject(new Error(`chrome did not report a DevTools endpoint:\n${chromeLog}`));
  }, 30_000);
  chrome.stderr.on('data', (chunk) => {
    chromeLog += chunk;
    const found = chromeLog.match(/DevTools listening on (ws:\/\/\S+)/);
    if (found) {
      clearTimeout(timer);
      resolve(found[1]);
    }
  });
  chrome.on('exit', (code) => {
    clearTimeout(timer);
    reject(new Error(`chrome exited (${code}) before the check ran:\n${chromeLog}`));
  });
});

const devtoolsPort = new URL(browserWs).port;

async function pageTarget() {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const list = await (await fetch(`http://127.0.0.1:${devtoolsPort}/json/list`)).json();
    const page = list.find((target) => target.type === 'page' && target.url.startsWith(origin));
    if (page) return page;
    await sleep(100);
  }
  throw new Error(`chrome never opened a page at ${origin}:\n${chromeLog}`);
}

// ---------------------------------------------------------------------------
// A minimal DevTools client: one id, one pending promise, one message handler.

class Cdp {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 0;
    this.pending = new Map();
    this.failures = [];
    socket.addEventListener('message', (event) => {
      const message = JSON.parse(event.data);
      if (message.id && this.pending.has(message.id)) {
        const { resolve, reject } = this.pending.get(message.id);
        this.pending.delete(message.id);
        if (message.error) reject(new Error(JSON.stringify(message.error)));
        else resolve(message.result);
      } else if (message.method === 'Runtime.exceptionThrown') {
        this.failures.push(message.params.exceptionDetails?.exception?.description ?? 'exception');
      }
    });
  }

  send(method, params = {}) {
    const id = (this.nextId += 1);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }

  async evaluate(expression) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (result.exceptionDetails) {
      throw new Error(
        `${result.exceptionDetails.text}: ${result.exceptionDetails.exception?.description ?? ''}`,
      );
    }
    return result.result.value;
  }
}

async function connect(url) {
  const socket = new WebSocket(url);
  await new Promise((resolve, reject) => {
    socket.addEventListener('open', resolve, { once: true });
    socket.addEventListener('error', () => reject(new Error(`cannot connect to ${url}`)), {
      once: true,
    });
  });
  return new Cdp(socket);
}

let exitCode = 2;
let cdp;
try {
  const page = await pageTarget();
  cdp = await connect(page.webSocketDebuggerUrl);
  await cdp.send('Runtime.enable');

  // The module is loaded asynchronously (`await init()` in demo.js), so wait for
  // the handle instead of racing it.
  let ready = false;
  for (let attempt = 0; attempt < 200 && !ready; attempt += 1) {
    ready = await cdp.evaluate('typeof window.rustim !== "undefined"');
    if (!ready) await sleep(100);
  }
  if (!ready) {
    const body = await cdp.evaluate('document.body.innerText');
    throw new Error(
      `window.rustim never appeared. exceptions: ${cdp.failures.join(' | ')}\npage text:\n${body}`,
    );
  }

  // Ten seconds of virtual time, then read the canvas back as a PNG. The comma
  // operator sequences the two so one round-trip does both.
  const dataUrl = await cdp.evaluate(
    'window.rustim.advance(10.0), document.getElementById("canvas").toDataURL("image/png")',
  );
  const failure = await cdp.evaluate('window.rustim.render_error()');
  if (failure !== undefined && failure !== null) {
    throw new Error(`the browser reported a render error: ${failure}`);
  }

  await mkdir(outDir, { recursive: true });
  const webFrame = join(outDir, 'web-frame.png');
  await writeFile(webFrame, Buffer.from(dataUrl.split(',')[1], 'base64'));

  // The binary's own still, from the same scene at the same virtual time.
  const still = join(outDir, 'web-still.png');
  const app = await run('cargo', [
    'run',
    '--release',
    '-p',
    'rustim-app',
    '--',
    '--scene',
    'pythagorean',
    '--png',
    still,
  ]);
  if (app !== 0) throw new Error(`rustim-app --png exited ${app}`);

  const compare = await run(
    'cargo',
    ['run', '--release', '-p', 'rustim-export', '--bin', 'rustim-compare', '--', webFrame, still],
    { show: true },
  );

  // The semantic tree, as Chrome actually built it. `getFullAXTree` is the
  // browser's own answer to "what can a screen reader see here", not a check of
  // the DOM we wrote — an `opacity: 0` overlay that Chrome pruned would show up
  // as a missing node, which is the failure this check exists to catch.
  await cdp.send('Accessibility.enable');
  const { nodes: axNodes } = await cdp.send('Accessibility.getFullAXTree');
  const visible = axNodes.filter((node) => !node.ignored);
  const named = (name) => visible.find((node) => node.name?.value === name);

  const figures = ['直角三角形', 'a 的正方形', 'b 的正方形', 'c 的正方形'];
  const missing = [];
  for (const figure of figures) {
    const node = named(figure);
    if (!node) {
      missing.push(`${figure} (absent from the AX tree)`);
      continue;
    }
    if (node.role?.value !== 'image') {
      missing.push(`${figure} (role ${node.role?.value}, expected image)`);
    }
    const description = node.description?.value ?? '';
    if (!description) missing.push(`${figure} (no description)`);
    if (description.includes('位置')) missing.push(`${figure} (description carries a position)`);
    console.log(
      `ax  image  ${figure}  —  ${node.description?.value ?? '(no description)'}`,
    );
  }
  const group = visible.find((node) => node.role?.value === 'group');
  if (!group) missing.push('the root group is absent from the AX tree');

  if (missing.length > 0) {
    console.error(`web-check: the accessibility tree is wrong:\n  ${missing.join('\n  ')}`);
    exitCode = 1;
  } else {
    exitCode = compare === 0 ? 0 : 1;
  }

  // The command bridge: one `add` returns the id it created, `render` puts it
  // on the canvas and into the overlay, and broken JSON is a refusal rather
  // than a thrown exception.
  const reply = JSON.parse(
    await cdp.evaluate(
      `window.rustim.apply('{"action":"add","type":"circle","params":{"radius":1.0,"label":"圆"}}')`,
    ),
  );
  const created = reply.diff?.created?.[0];
  if (typeof created !== 'number') {
    console.error(`web-check: the add was not answered with an id: ${JSON.stringify(reply)}`);
    exitCode = 1;
  }
  const figuresAfter = await cdp.evaluate(
    'window.rustim.render(), document.querySelectorAll("#a11y [role=img]").length',
  );
  if (figuresAfter !== figures.length + 1) {
    console.error(`web-check: the DOM overlay has ${figuresAfter} figures, expected ${figures.length + 1}`);
    exitCode = 1;
  }
  const refused = JSON.parse(await cdp.evaluate(`window.rustim.apply('{')`));
  if (refused.error === undefined) {
    console.error(`web-check: broken JSON was not refused: ${JSON.stringify(refused)}`);
    exitCode = 1;
  }
  console.log(`apply  created ${created}, ${figuresAfter} figures in the overlay, refusal ok`);

  // Pointer gestures, through `Input.dispatchMouseEvent` rather than
  // `dispatchEvent`: Chrome builds the real pointer/wheel events from this, so
  // the shell's own listeners are exercised rather than a synthetic event that
  // happens to have the fields we chose.
  const box = await cdp.evaluate(`(() => {
    const canvas = document.getElementById("canvas");
    const rect = canvas.getBoundingClientRect();
    return { x: rect.x, y: rect.y, w: rect.width, h: rect.height,
             cw: canvas.width, ch: canvas.height };
  })()`);
  // A CSS pixel is `canvas.width / rect.width` canvas pixels. The page caps the
  // canvas at `max-width: 100%`, so this is not always 1.
  const scale = box.cw / box.w;
  const start = { x: Math.round(box.x + box.w * 0.3), y: Math.round(box.y + box.h * 0.5) };
  const end = { x: start.x + 200, y: start.y };
  const cameraBefore = await cdp.evaluate('[...window.rustim.camera()]');

  const mouse = (type, x, y, extra = {}) =>
    cdp.send('Input.dispatchMouseEvent', { type, x, y, ...extra });

  // Hover, press, one move, release — the sequence a real drag produces. The
  // hover matters: it establishes the anchor before the button goes down.
  await mouse('mouseMoved', start.x, start.y);
  await mouse('mousePressed', start.x, start.y, { button: 'left', buttons: 1, clickCount: 1 });
  await mouse('mouseMoved', end.x, end.y, { button: 'left', buttons: 1 });
  await mouse('mouseReleased', end.x, end.y, { button: 'left', buttons: 0, clickCount: 1 });
  await sleep(100);
  const dragged = await cdp.evaluate('[...window.rustim.camera()]');

  const dragPx = 200 * scale;
  const perUnit = (box.ch / 8) * cameraBefore[2];
  const expectedCenterX = cameraBefore[0] - dragPx / perUnit;
  const dragOk =
    Math.abs(dragged[0] - expectedCenterX) < 1e-9 &&
    dragged[1] === cameraBefore[1] &&
    dragged[2] === cameraBefore[2];
  if (!dragOk) {
    console.error(
      `web-check: a ${dragPx} px drag moved the centre to ${dragged[0]}, expected ${expectedCenterX}`,
    );
    exitCode = 1;
  } else {
    console.log(
      `gesture  drag ${dragPx} canvas px  ->  centre.x ${dragged[0]} (expected ${expectedCenterX})`,
    );
  }

  // A -100 px DOM wheel is 100 px "away from the user": `exp(100 * 0.002)`.
  await mouse('mouseWheel', end.x, end.y, { deltaX: 0, deltaY: -100, button: 'none' });
  await sleep(100);
  const zoomed = await cdp.evaluate('[...window.rustim.camera()]');

  const expectedZoom = dragged[2] * Math.exp(0.2);
  const zoomOk = Math.abs(zoomed[2] - expectedZoom) < 1e-9;
  // The world point under the cursor must not move. Recomputed independently
  // from the projection's own definition, not from the engine's helper.
  const px = (end.x - box.x) * scale;
  const py = (end.y - box.y) * scale;
  const under = (camera) => {
    const s = (box.ch / 8) * camera[2];
    return [(px - box.cw / 2) / s + camera[0], camera[1] + (box.ch / 2 - py) / s];
  };
  const anchorBefore = under(dragged);
  const anchorAfter = under(zoomed);
  const anchorMoved = Math.hypot(
    anchorAfter[0] - anchorBefore[0],
    anchorAfter[1] - anchorBefore[1],
  );
  if (!zoomOk || anchorMoved > 1e-9) {
    console.error(
      `web-check: the wheel zoomed to ${zoomed[2]}, expected ${expectedZoom}; ` +
        `the anchor moved ${anchorMoved}`,
    );
    exitCode = 1;
  } else {
    console.log(
      `gesture  wheel -100 px  ->  zoom ${zoomed[2]} (expected ${expectedZoom}); ` +
        `anchor moved ${anchorMoved}`,
    );
  }

  // -------------------------------------------------------------------------
  // Keyboard, through `Input.dispatchKeyEvent` rather than a synthetic
  // `KeyboardEvent`: it goes through Chrome's own input pipeline, so the
  // listeners see a real key arriving at a real focus target.
  //
  // What is asserted is the **engine's** state — the session block a `query`
  // reports, which is the same one an AI reads back — rather than the page's
  // opinion of itself.

  const key = async (k, code, vk) => {
    const common = { key: k, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk };
    await cdp.send('Input.dispatchKeyEvent', {
      type: 'keyDown',
      ...common,
      ...(k.length === 1 ? { text: k, unmodifiedText: k } : {}),
    });
    await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...common });
  };
  const session = async () =>
    JSON.parse(await cdp.evaluate(`window.rustim.apply('{"action":"query"}')`)).session;
  const frame = () => cdp.evaluate('document.getElementById("canvas").toDataURL("image/png")');

  // The figures are focusable, and each carries the node it stands for. That is
  // what lets a key be *about* one of them, and it is why this host has no
  // `FocusState`: the browser owns the focus ring.
  const focused = await cdp.evaluate(`(() => {
    const el = document.querySelector('[data-rustim-node][tabindex]');
    if (!el) return null;
    el.focus();
    return document.activeElement === el ? el.getAttribute('data-rustim-node') : null;
  })()`);

  // ...and Tab is the browser's own order over them, not a reimplementation.
  await key('Tab', 'Tab', 9);
  const tabbedTo = await cdp.evaluate(
    `document.activeElement && document.activeElement.getAttribute('data-rustim-node')`,
  );

  // Space stops the timeline.
  await key(' ', 'Space', 32);
  await sleep(50);
  const stopped = await session();

  // `m` flips the motion policy — the other toggle the browser honours.
  await key('m', 'KeyM', 77);
  await sleep(50);
  const flipped = await session();

  // An arrow moves the *focused* figure's opacity. The timeline is paused by
  // then, so nothing else can move: a canvas that changed is the arrow's doing,
  // and rendering twice with no key between is the counterfactual that proves
  // the comparison can tell.
  await cdp.evaluate('window.rustim.render()');
  const idle = await frame();
  const alsoIdle = await frame();
  await key('ArrowLeft', 'ArrowLeft', 37);
  await key('ArrowLeft', 'ArrowLeft', 37);
  await key('ArrowLeft', 'ArrowLeft', 37);
  await cdp.evaluate('window.rustim.render()');
  const pressed = await frame();

  const keyboardOk = focused !== null && tabbedTo !== null && tabbedTo !== focused;
  const togglesOk = stopped.paused === true && flipped.motion === 'reduced';
  const arrowOk = idle === alsoIdle && pressed !== idle;

  if (!keyboardOk || !togglesOk || !arrowOk) {
    console.error(
      `web-check: keyboard — focused ${focused}, Tab reached ${tabbedTo}; ` +
        `space gave paused=${stopped.paused}, m gave motion=${flipped.motion}; ` +
        `idle frames equal=${idle === alsoIdle}, an arrow changed the canvas=${pressed !== idle}`,
    );
    exitCode = 1;
  } else {
    console.log(
      `keys  Tab reached node ${tabbedTo} (focused ${focused}); ` +
        `space -> paused, m -> ${flipped.motion}, arrows changed the canvas`,
    );
  }

  // Recording the canvas, which is the **page's** feature rather than the
  // engine's export path. `VideoSink` writes frames with the virtual clock's
  // timestamps and is reproducible frame for frame; `MediaRecorder` records
  // whatever the canvas shows while wall-clock time passes, so the same scene
  // twice gives two files. Asserted on the **bytes**, because a recording that
  // produced nothing resolves successfully with an empty blob and would otherwise
  // read as a working download.
  const recorded = await cdp.evaluate(`(async () => {
    const ticking = setInterval(() => window.rustim.advance(1 / 30), 33);
    const out = await window.rustimRecord(1.0, 30);
    clearInterval(ticking);
    return { bytes: out.bytes, type: out.type, chunks: out.chunks, error: out.error };
  })()`);

  const recordedOk =
    recorded.bytes > 0 && String(recorded.type ?? '').startsWith('video/');
  if (!recordedOk) {
    console.error(`web-check: recording produced ${JSON.stringify(recorded)}`);
    exitCode = 1;
  } else {
    console.log(
      `record  ${recorded.bytes} bytes of ${recorded.type} in ${recorded.chunks} chunk(s)`,
    );
  }

  // The second artifact. Typst is linked only into `pkg-formula`, and this is
  // the only place its wasm build is actually *run* rather than compiled.
  await cdp.send('Page.navigate', { url: `${origin}/formula.html?paused=1` });
  let onFormula = false;
  for (let attempt = 0; attempt < 600 && !onFormula; attempt += 1) {
    await sleep(100);
    try {
      onFormula = await cdp.evaluate(
        'location.pathname.endsWith("formula.html") && typeof window.rustim !== "undefined"',
      );
    } catch {
      // The execution context is being replaced by the navigation; ask again.
    }
  }
  if (!onFormula) throw new Error('the formula page never became ready');

  const math = await cdp.evaluate(
    'window.rustim.advance(1.0), document.querySelector("#a11y [role=math] math")?.outerHTML.slice(0, 200)',
  );
  if (!math || !math.includes('<mfrac')) {
    console.error(`web-check: no structural <math> in the overlay: ${math ?? '(absent)'}`);
    exitCode = 1;
  } else {
    console.log(`math   ${math}`);
  }

  const { nodes: formulaAx } = await cdp.send('Accessibility.getFullAXTree');
  const mathNode = formulaAx
    .filter((node) => !node.ignored)
    .find((node) => node.role?.value === 'math' && node.name?.value === '公式');
  if (!mathNode) {
    console.error('web-check: Chrome exposes no math node named 公式');
    exitCode = 1;
  } else {
    console.log('ax     math  公式');
  }
} catch (error) {
  console.error(`web-check: ${error.message}`);
  exitCode = 2;
} finally {
  if (cdp && keepOpen) {
    console.log(`web-check: leaving the browser up at ${origin}`);
  } else {
    server.close();
    chrome.kill('SIGKILL');
  }
}

if (!keepOpen) process.exit(exitCode);

function run(command, args, { show = false } = {}) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd: root,
      stdio: ['ignore', show ? 'inherit' : 'ignore', 'inherit'],
    });
    child.on('exit', (code) => resolve(code ?? 1));
  });
}
