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
//     broken JSON.
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
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { extname, join, normalize, sep } from 'node:path';
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

const chromePath = process.env.CHROME || 'google-chrome';
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
