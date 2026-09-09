// Real Chrome + shipped fmn Studio acceptance. No browser framework or mock
// server. Build fmn through RCH first, then pass its exact executable path.
// Usage: /usr/bin/node browser.mjs /absolute/fmn /absolute/evidence-directory
import {spawn, execFileSync} from "node:child_process";
import {mkdir, mkdtemp, readFile, writeFile} from "node:fs/promises";
import {createHash} from "node:crypto";
import {tmpdir} from "node:os";
import {join, resolve} from "node:path";
import {once} from "node:events";
import assert from "node:assert/strict";

const binary = resolve(process.argv[2]), output = resolve(process.argv[3]);
const chrome = process.env.FMN_CHROME || "/usr/bin/google-chrome";
const userAgent = "OpenAI File Downloader, XaiImageApiFetch/1.0";
await mkdir(output, {recursive:true});
const profile = await mkdtemp(join(tmpdir(), "fmn-studio-browser-"));
const secrets = [];
const redact = text => secrets.reduce((s, secret) => s.split(secret).join("[session]"), String(text));
const sleep = ms => new Promise(r => setTimeout(r, ms));
async function until(fn, label, milliseconds = 30000) {
  const end = Date.now() + milliseconds;
  while (Date.now() < end) { const value = await fn(); if (value) return value; await sleep(50); }
  throw new Error(`Timed out: ${label}`);
}
async function stop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.stdin?.end();
  const stopped = once(child,"exit");
  const timer = setTimeout(() => child.kill("SIGTERM"), 5000);
  const killTimer = setTimeout(() => child.kill("SIGKILL"), 10000);
  try { await stopped; } finally { clearTimeout(timer); clearTimeout(killTimer); }
}
async function startStudio(scene) {
  const child = spawn(binary, ["studio","--robot","--no-browser","--resolution","384x216","--fps","8","--threads","1","@builtin",scene],
    {stdio:["pipe","pipe","pipe"], env:{...process.env, PYTHONHOME:"", PYTHONPATH:""}});
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", chunk => { stdout += chunk; if (stdout.length > 65536) child.kill("SIGTERM"); });
  child.stderr.on("data", chunk => { stderr = (stderr + chunk).slice(-65536); });
  try {
    const ready = await until(() => {
      if (child.exitCode !== null) throw new Error(`Studio exited ${child.exitCode}: ${redact(stderr)}`);
      for (const line of stdout.split("\n")) { try { const data = JSON.parse(line); if (data.kind === "studio_ready") return data; } catch {} }
    }, "native Studio ready");
    const url = new URL(ready.url), cap = url.searchParams.get("cap");
    assert.equal(url.protocol,"http:"); assert.ok(["127.0.0.1","[::1]"].includes(url.hostname));
    assert.ok(cap); secrets.push(cap);
    return {child, url, cap, stderr:() => stderr};
  } catch (error) { await stop(child); throw error; }
}

const browser = spawn(chrome, ["--headless=new","--disable-gpu","--disable-dev-shm-usage","--no-first-run","--no-default-browser-check",
  "--remote-debugging-port=0",`--user-data-dir=${profile}`,`--user-agent=${userAgent}`,"about:blank"], {stdio:["ignore","ignore","pipe"]});
browser.stderr.resume();
let ws;
let studio;
const receipt = {schema:"fmn.studio.browser.v1", source_commit:execFileSync("git",["rev-parse","HEAD"],{encoding:"utf8"}).trim(),
  source_status:execFileSync("git",["status","--porcelain","--untracked-files=no"],{encoding:"utf8"}).trim(),
  binary_sha256:createHash("sha256").update(await readFile(binary)).digest("hex"), scenarios:[], browser_errors:[]};
try {
  const port = await until(async () => { try { return Number((await readFile(join(profile,"DevToolsActivePort"),"utf8")).split("\n")[0]); } catch { return null; } }, "Chrome DevTools port");
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`, {headers:{"User-Agent":userAgent},signal:AbortSignal.timeout(10000)})).json();
  ws = new WebSocket(targets.find(t => t.type === "page").webSocketDebuggerUrl); await once(ws,"open");
  let next = 0; const pending = new Map();
  ws.addEventListener("message", event => {
    const m = JSON.parse(event.data);
    if (m.id && pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); clearTimeout(p.timer); if (m.error) p.reject(new Error(m.error.message)); else p.resolve(m.result); }
    // ubs:ignore — this compares a public CDP event name, not an authentication secret.
    if (m.method === "Runtime.exceptionThrown") receipt.browser_errors.push(redact(m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text));
  });
  function command(method, params = {}) {
    return new Promise((resolve, reject) => { const id = ++next;
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 30000);
      pending.set(id,{resolve,reject,timer}); ws.send(JSON.stringify({id,method,params}));
    });
  }
  async function evaluate(expression) {
    const result = await command("Runtime.evaluate", {expression, awaitPromise:true, returnByValue:true});
    if (result.exceptionDetails) throw new Error(redact(result.exceptionDetails.exception?.description || result.exceptionDetails.text));
    return result.result.value;
  }
  async function click(selector) {
    const point = await evaluate(`(() => { const element=document.querySelector(${JSON.stringify(selector)}); element.scrollIntoView({block:"center"}); const r=element.getBoundingClientRect(); return {x:r.x+r.width/2,y:r.y+r.height/2}; })()`);
    await command("Input.dispatchMouseEvent", {type:"mousePressed",button:"left",clickCount:1,...point});
    await command("Input.dispatchMouseEvent", {type:"mouseReleased",button:"left",clickCount:1,...point});
  }
  async function key(key, code, virtualKey) {
    await command("Input.dispatchKeyEvent", {type:"keyDown",key,code,windowsVirtualKeyCode:virtualKey});
    await command("Input.dispatchKeyEvent", {type:"keyUp",key,code,windowsVirtualKeyCode:virtualKey});
  }
  async function synchronized(frame) {
    await until(() => evaluate(`document.getElementById("display").dataset.synchronized === "true" && document.getElementById("display").dataset.frame === ${JSON.stringify(String(frame))} && !document.getElementById("inspect").disabled`), `frame ${frame} synchronized`);
  }
  async function inspect() {
    return evaluate(`fetch("/api/inspect",{headers:{"X-FMN-Capability":${JSON.stringify(studio.cap)}},signal:AbortSignal.timeout(10000)}).then(r=>r.json())`);
  }
  async function screenshot(name) { const result = await command("Page.captureScreenshot",{format:"png",captureBeyondViewport:true}); await writeFile(join(output,name),Buffer.from(result.data,"base64")); }
  await command("Page.enable"); await command("Runtime.enable");
  await command("Emulation.setDeviceMetricsOverride",{width:1280,height:1000,deviceScaleFactor:1,mobile:false});
  for (const scene of ["circle_shift.v1","tex_span.v1"]) {
    studio = await startStudio(scene);
    await command("Page.navigate",{url:studio.url.href});
    await synchronized(0);
    const begin = await inspect(), last = begin.view.frame_count - 1;
    assert.ok(last >= 1);
    assert.equal(begin.view.fps,30,"native windowed clock overrides requested FPS");
    assert.ok((await evaluate("document.getElementById('position').textContent")).includes(begin.scene_time.toFixed(3) + " s"));
    assert.equal(await evaluate("location.search"), "");
    assert.equal(await evaluate("document.getElementById('input-events').disabled"), true);
    assert.match(await evaluate("document.getElementById('input-support').textContent"), /no live scene input adapter/);
    const initialHash = await evaluate("document.getElementById('display').dataset.sha256");
    await click("#last"); await synchronized(last);
    const end = await inspect();
    if (scene === "circle_shift.v1") {
      assert.notDeepEqual(end.nodes, begin.nodes, "animated live records must change");
      assert.notEqual(await evaluate("document.getElementById('display').dataset.sha256"), initialHash, "rendered frame must change");
      // Affine animation updates the live placement; the shared local record
      // geometry correctly remains unchanged until a geometry edit occurs.
      const moving = end.nodes.find(node => JSON.stringify(node.placement) !== JSON.stringify(begin.nodes.find(n=>n.id===node.id)?.placement));
      assert.ok(moving);
      await click(`[data-node="${moving.id}"] > .tree-label`);
      assert.ok((await evaluate("document.getElementById('details').textContent")).includes(String(moving.records.fields.find(field=>field.name==="point").values[0])));
      await click("#details details:nth-last-child(2) > summary");
      assert.ok((await evaluate("document.querySelector('#details details:nth-last-child(2)').innerText")).includes(String(moving.placement.translation[0])));
      await screenshot(scene + ".inspection.png");
    } else {
      const glyph = end.nodes.find(n => n.source_span?.kind === "math_glyph" && n.source_span.excerpt === "x"); assert.ok(glyph);
      assert.ok(glyph.parents.length > 0, "select a nested glyph");
      await evaluate(`document.querySelector('[data-node="${glyph.id}"]').scrollIntoView({block:"center"})`);
      await click(`[data-node="${glyph.id}"] > .tree-label`);
      assert.match(await evaluate("document.getElementById('details').textContent"), /UTF-8 bytes 6\.\.7/);
      await screenshot(scene + ".inspection.png");
      assert.equal(await evaluate("document.activeElement.getAttribute('role')"), "treeitem");
      await key("Home","Home",36);
      const first = await evaluate("document.activeElement.dataset.node");
      await key("ArrowDown","ArrowDown",40);
      assert.notEqual(await evaluate("document.activeElement.dataset.node"), first, "keyboard traverses nested family");
    }
    await evaluate("document.getElementById('timeline').value='0'; document.getElementById('timeline').dispatchEvent(new Event('input',{bubbles:true}))");
    await synchronized(0);
    assert.match(await evaluate("document.getElementById('replay').textContent"), /Previewing frame 0/);
    const beforeRestart = await evaluate("Number(document.getElementById('display').dataset.publication)");
    await click("#restart"); await synchronized(last);
    await until(() => evaluate(`Number(document.getElementById('display').dataset.publication) > ${beforeRestart}`), "restarted worker publishes a new PNG part");
    assert.match(await evaluate("document.getElementById('worker').textContent"), /generation/);
    assert.match(await evaluate("document.getElementById('replay').textContent"), /Restart restored/);
    await evaluate(`(() => { const slider=document.getElementById('timeline'); for(const frame of [0,1,${last}]) { slider.value=String(frame); slider.dispatchEvent(new Event('input',{bubbles:true})); } slider.dispatchEvent(new Event('change',{bubbles:true})); })()`);
    await synchronized(last);
    assert.match(await evaluate("document.getElementById('replay').textContent"), new RegExp(`Committed frame ${last}`));
    const overlayNode = end.nodes.find(node => node.records.count > 0);
    await click(`[data-node="${overlayNode.id}"] > .tree-label`);
    for (const value of [1,2,4,8,16]) {
      await click(`#layers input[value="${value}"]`);
      await until(() => evaluate("!document.getElementById('inspect').disabled && Number(document.getElementById('overlay-state').dataset.marks)>0"), `overlay ${value}`);
      assert.ok(await evaluate("document.getElementById('overlay').getContext('2d').getImageData(0,0,384,216).data.some(value=>value!==0)"));
      await click(`#layers input[value="${value}"]`);
      await until(() => evaluate("!document.getElementById('inspect').disabled && document.getElementById('overlay-state').textContent==='Overlays off.'"), "overlay off");
    }
    await click('#layers input[value="4"]');
    await until(() => evaluate("!document.getElementById('inspect').disabled && Number(document.getElementById('overlay-state').dataset.marks)>0"), "bounds overlay");
    await screenshot(scene + ".desktop.png");
    await command("Emulation.setDeviceMetricsOverride",{width:390,height:844,deviceScaleFactor:1,mobile:false});
    assert.equal(await evaluate("document.documentElement.scrollWidth <= innerWidth"), true, "compact viewport has no horizontal overflow");
    await evaluate("document.getElementById('timeline').focus()"); await key("Home","Home",36); await synchronized(0);
    await key("End","End",35); await synchronized(last);
    await screenshot(scene + ".compact.png");
    const rejections = [];
    for (const [fields, expected] of [[{frame:"-1",commit:"true"},400],[{frame:"banana",commit:"true"},400],[{frame:String(begin.view.frame_count),commit:"false"},422]]) {
      const response = await fetch(new URL("/api/scrub", studio.url), {method:"POST", headers:{"User-Agent":userAgent,"X-FMN-Capability":studio.cap,"Origin":studio.url.origin,"Content-Type":"application/x-www-form-urlencoded"},body:new URLSearchParams(fields),signal:AbortSignal.timeout(10000)});
      const body = await response.text(); assert.equal(response.status,expected); assert.ok(body.length < 8192); assert.ok(!body.includes(studio.cap)); rejections.push(response.status);
    }
    const denied = await fetch(new URL("/api/inspect", studio.url),{headers:{"User-Agent":userAgent,"X-FMN-Capability":"0".repeat(64)},signal:AbortSignal.timeout(10000)});
    assert.equal(denied.status,403); assert.ok(!(await denied.text()).includes(studio.cap));
    const event = await fetch(new URL("/api/event", studio.url),{method:"POST",headers:{"User-Agent":userAgent,"X-FMN-Capability":studio.cap,"Origin":studio.url.origin,"Content-Type":"application/x-www-form-urlencoded"},body:"type=key_press&key=x",signal:AbortSignal.timeout(10000)});
    assert.equal(event.status,422); assert.match(await event.text(),/no live command\/event adapter/);
    // A real typed refusal must be useful in the page, too: inject an invalid
    // control value then submit through the actual installed event handler.
    await evaluate(`document.getElementById('frame').value=${JSON.stringify(String(begin.view.frame_count))}; document.getElementById('timeline-form').dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}))`);
    assert.match(await evaluate("document.getElementById('error').textContent"), /Choose an integer frame/);
    assert.equal(await evaluate(`document.body.innerText.includes(${JSON.stringify(studio.cap)})`),false);
    receipt.scenarios.push({scene,frame_count:begin.view.frame_count,nested_nodes:begin.nodes.filter(n=>n.parents.length).length,
      committed_restart_frame:last,rejections,overlay_marks:await evaluate("Number(document.getElementById('overlay-state').dataset.marks)"),keyboard:true,compact:true});
    await command("Page.navigate",{url:"about:blank"}); await stop(studio.child);
    assert.equal(studio.child.exitCode,0); assert.equal(studio.stderr(),""); studio = null;
    await command("Emulation.setDeviceMetricsOverride",{width:1280,height:1000,deviceScaleFactor:1,mobile:false});
  }
  assert.deepEqual(receipt.browser_errors,[]);
  receipt.passed = true;
} catch (error) {
  receipt.passed = false; receipt.error = redact(error.stack || error); process.exitCode = 1;
} finally {
  if (studio) await stop(studio.child);
  ws?.close(); browser.kill("SIGTERM"); await stop(browser);
  await writeFile(join(output,"receipt.json"),JSON.stringify(receipt,null,2)+"\n");
  process.stdout.write(JSON.stringify(receipt)+"\n");
}
