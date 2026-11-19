// The KWin script sopen loads after opening a folder or file in an editor on KDE Plasma
// (src/raise.rs), run against a stand-in `workspace` with signals. src/raise.rs's tests write the
// scripts and run this with node:
//
//   node kwin-script.mjs <dir>
//
// <dir> holds `code.js` (VS Code's script for the opened name "project"), `zed.js` (Zed's, same
// name), `names.json` (odd names) and `odd-<i>.js` (VS Code's script for each odd name).
// Prints one line per check; exits 1 when any fails.

import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';

const dir = process.argv[2];
const read = (name) => fs.readFileSync(path.join(dir, name), 'utf8');

class Signal {
  constructor() {
    this.handlers = [];
  }
  connect(f) {
    this.handlers.push(f);
  }
  disconnect(f) {
    const i = this.handlers.indexOf(f);
    if (i >= 0) this.handlers.splice(i, 1);
  }
  emit(...args) {
    for (const f of [...this.handlers]) f(...args);
  }
}

// Desktops all named alike, as a session may have them: only the objects tell them apart.
const D1 = { name: 'Desktop 5' };
const D5a = { name: 'Desktop 5' };
const D5b = { name: 'Desktop 5' };
const EDP = { name: 'eDP-1' };
const DP2 = { name: 'DP-2' };

class Win {
  constructor(resourceClass, caption, more = {}) {
    this.resourceClass = resourceClass;
    this.caption = caption;
    this.demandsAttention = false;
    this.onAllDesktops = false;
    this.minimized = false;
    this.desktops = [D1];
    this.output = EDP;
    Object.assign(this, more);
    this.demandsAttentionChanged = new Signal();
    this.captionChanged = new Signal();
  }
  demand(on) {
    this.demandsAttention = on;
    this.demandsAttentionChanged.emit();
  }
  retitle(caption) {
    this.caption = caption;
    this.captionChanged.emit();
  }
  untouched() {
    return this.desktops.length === 1 && this.desktops[0] === D1 && this.output === EDP;
  }
}

/// A workspace over `windows` with `active` as the active window; runs `script` in it.
function world(script, windows, active) {
  const ws = {
    windowAdded: new Signal(),
    activations: [],
    sent: [],
    _active: active,
    windowList() {
      return [...windows];
    },
    get activeWindow() {
      return this._active;
    },
    set activeWindow(w) {
      this._active = w;
      this.activations.push(w);
      w.demandsAttention = false;
    },
    get currentDesktop() {
      return D5a;
    },
    set currentDesktop(_) {
      throw new Error('the script set workspace.currentDesktop');
    },
    activeScreen: EDP,
    sendClientToScreen(w, output) {
      this.sent.push([w, output]);
      w.output = output;
    },
    add(w) {
      windows.push(w);
      this.windowAdded.emit(w);
    },
    // Handlers still connected anywhere.
    connected() {
      return [this.windowAdded, ...windows.flatMap((w) => [w.demandsAttentionChanged, w.captionChanged])]
        .reduce((n, s) => n + s.handlers.length, 0);
    },
  };
  vm.runInNewContext(script, { workspace: ws });
  return ws;
}

let failed = 0;
function check(name, ok) {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
  if (!ok) failed++;
}

const code = read('code.js');
const shown = 'README.md - project [SSH: box] - Visual Studio Code';
const stale = 'notes.md - other [SSH: box] - Visual Studio Code';

{
  // The opened window (A) and an older demand (B, newer window): only A comes; after that,
  // nothing more happens, whatever B or a new window does.
  const term = new Win('konsole', '~ : zsh', { desktops: [D5b], output: DP2 });
  const a = new Win('code', shown, { demandsAttention: true, minimized: true });
  const b = new Win('code', stale, { demandsAttention: true });
  const ws = world(code, [a, term, b], term);
  check('caption: A activated, once', ws.activations.length === 1 && ws.activations[0] === a);
  check('caption: A un-minimized', a.minimized === false);
  check('caption: A on the active window\'s desktop object', a.desktops === term.desktops && a.desktops[0] === D5b);
  check('caption: A sent to the active window\'s screen', ws.sent.length === 1 && ws.sent[0][1] === DP2);
  check('caption: B untouched', b.untouched());
  check('caption: nothing left connected', ws.connected() === 0);
  b.demand(false);
  b.demand(true);
  ws.add(new Win('code', 'new - Visual Studio Code'));
  check('done: B asking again, a new window: ignored', ws.activations.length === 1 && b.untouched());
}
{
  // No window shows the name at load: B's old demand is ignored; C asking now is the one.
  const term = new Win('konsole', '~ : zsh', { desktops: [D5b], output: DP2 });
  const b = new Win('code', stale, { demandsAttention: true });
  const c = new Win('code', 'other window - Visual Studio Code');
  const ws = world(code, [c, term, b], term);
  check('stale: nothing at load', ws.activations.length === 0 && b.untouched());
  c.demand(true);
  check('demand later: C activated', ws.activations.length === 1 && ws.activations[0] === c);
  check('demand later: C moved', c.desktops[0] === D5b && c.output === DP2);
  check('demand later: B untouched', b.untouched());
  check('demand later: nothing left connected', ws.connected() === 0);
  b.demand(false);
  b.demand(true);
  check('demand later: then nothing', ws.activations.length === 1);
}
{
  // A window asking at load whose title shows the name only later: activated then.
  const a = new Win('code', 'Visual Studio Code', { demandsAttention: true });
  const ws = world(code, [a], null);
  check('title later: nothing at load', ws.activations.length === 0);
  a.retitle(shown);
  check('title later: activated', ws.activations.length === 1 && ws.activations[0] === a);
  check('title later: current desktop, no screen change', a.desktops[0] === D5a && ws.sent.length === 0);
}
{
  // A new editor window (a folder not open yet) is the one; another app's is not.
  const term = new Win('konsole', '~ : zsh', { onAllDesktops: true, output: DP2 });
  const ws = world(code, [term], term);
  ws.add(new Win('firefox', 'project - Mozilla Firefox'));
  check('new window: another app ignored', ws.activations.length === 0);
  const fresh = new Win('code', 'Visual Studio Code');
  ws.add(fresh);
  check('new window: activated', ws.activations.length === 1 && ws.activations[0] === fresh);
  check('new window: anchor on all desktops -> current desktop', fresh.desktops[0] === D5a);
  check('new window: nothing left connected', ws.connected() === 0);
}
{
  // Nothing qualifies: another app showing the name, a stale demand, a quiet editor window.
  const ff = new Win('firefox', 'project - Mozilla Firefox', { demandsAttention: true });
  const b = new Win('code', stale, { demandsAttention: true });
  const quiet = new Win('code', shown);
  const ws = world(code, [ff, b, quiet], null);
  quiet.retitle(shown + ' ');
  check('nothing qualifies: nothing changes', ws.activations.length === 0 && ws.sent.length === 0
    && ff.untouched() && b.untouched() && quiet.untouched());
}
{
  // The editor's own window on all desktops stays so.
  const term = new Win('konsole', '~ : zsh', { desktops: [D5b] });
  const a = new Win('code', shown, { demandsAttention: true, onAllDesktops: true });
  const ws = world(code, [term, a], term);
  check('on all desktops: kept, activated', a.desktops[0] === D1 && ws.activations[0] === a);
}
{
  // Of two windows showing the name and asking, the newer.
  const old = new Win('code', shown, { demandsAttention: true });
  const newer = new Win('code', shown, { demandsAttention: true });
  const ws = world(code, [old, newer], null);
  check('newest first', ws.activations.length === 1 && ws.activations[0] === newer && old.untouched());
}
{
  // Zed's classes.
  const zed = read('zed.js');
  for (const [cls, hit] of [['dev.zed.Zed', true], ['zed', true], ['dev.zed.Zed-Preview', true], ['zedmon', false], ['code', false]]) {
    const w = new Win(cls, 'project - Zed', { demandsAttention: true });
    const ws = world(zed, [w], null);
    check(`zed class ${cls}: ${hit ? 'activated' : 'ignored'}`, (ws.activations[0] === w) === hit);
  }
}
{
  // Odd opened names reach the script as they are: matched in a title, nothing else run.
  const names = JSON.parse(read('names.json'));
  names.forEach((name, i) => {
    const script = read(`odd-${i}.js`);
    const a = new Win('code', `${name} - Visual Studio Code`, { demandsAttention: true });
    const b = new Win('code', stale, { demandsAttention: true });
    const ws = world(script, [a, b], null);
    check(`odd name ${JSON.stringify(name)}`, ws.activations.length === 1 && ws.activations[0] === a && b.untouched());
  });
}

console.log(failed ? `${failed} FAILED` : 'all ok');
process.exit(failed ? 1 : 0);
