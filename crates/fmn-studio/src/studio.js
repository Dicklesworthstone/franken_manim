"use strict";
(() => {
  const script = document.currentScript;
  const capability = new URL(script.src).searchParams.get("cap");
  const $ = id => document.getElementById(id);
  if (!capability) { $("error").hidden = false; $("error").textContent = "Missing Studio session capability."; return; }
  const headers = {"X-FMN-Capability": capability};
  const css = document.createElement("link");
  css.rel = "stylesheet";
  css.href = "/studio.css?cap=" + encodeURIComponent(capability);
  document.head.append(css);
  history.replaceState(null, "", "/");
  const MAX_JSON = 8 * 1024 * 1024, MAX_PNG = 64 * 1024 * 1024;
  const state = {snapshot: null, overlay: null, selected: null, collapsed: new Set(),
    shown: null, expected: null, busy: false, pending: null, refreshNeeded: false, playing: false, connected: false};
  const safeText = value => String(value).split(capability).join("[session]").slice(0, 600);
  function report(error) {
    $("error").textContent = safeText(error.message || error);
    $("error").hidden = false;
  }
  function clearError() { $("error").hidden = true; $("error").textContent = ""; }
  async function boundedBody(response, limit) {
    const reader = response.body.getReader(), chunks = [];
    let length = 0;
    try {
      for (;;) {
        const {done, value} = await reader.read();
        if (done) break;
        length += value.length;
        if (length > limit) throw new Error("Studio response exceeds the browser payload budget.");
        chunks.push(value);
      }
      const bytes = new Uint8Array(length);
      let at = 0;
      for (const chunk of chunks) { bytes.set(chunk, at); at += chunk.length; }
      return new TextDecoder("utf-8", {fatal: true}).decode(bytes);
    } finally { await reader.cancel().catch(() => {}); }
  }
  async function api(path, fields) {
    const response = await fetch(path, {headers: fields ? {...headers,
      "Content-Type": "application/x-www-form-urlencoded"} : headers,
      method: fields ? "POST" : "GET", body: fields ? new URLSearchParams(fields) : undefined,
      signal: AbortSignal.timeout(30000)});
    const text = await boundedBody(response, response.ok ? MAX_JSON : 8192);
    let data;
    try { data = JSON.parse(text); } catch { if (response.ok) throw new Error("Invalid Studio JSON response."); }
    if (!response.ok) throw new Error(`Studio ${response.status}: ${safeText(data?.message || text)}`);
    return data;
  }
  function viewOf(snapshot) {
    const v = snapshot?.view;
    if (snapshot?.version !== 1 || !Number.isFinite(snapshot.scene_time) || !Array.isArray(snapshot.nodes) || snapshot.nodes.length > 50000 ||
        !v || !Number.isSafeInteger(v.frame_count) || v.frame_count < 1 ||
        !Number.isSafeInteger(v.frame_index) || v.frame_index < 0 || v.frame_index >= v.frame_count ||
        !Number.isInteger(v.fps) || v.fps < 1 || !Number.isInteger(v.width) || !Number.isInteger(v.height) ||
        v.width < 1 || v.height < 1 || v.width * v.height > 33554432 ||
        !Number.isFinite(v.scale) || v.scale <= 0 || !Array.isArray(v.origin) ||
        v.origin.length !== 2 || !v.origin.every(Number.isFinite) || typeof v.input_events !== "boolean") {
      throw new Error("Worker returned unsupported or invalid inspector view metadata.");
    }
    return v;
  }
  function matchesFrame() {
    return state.connected && state.snapshot && state.shown &&
      state.snapshot.view.frame_index === state.shown.index &&
      state.snapshot.view.width === state.shown.width && state.snapshot.view.height === state.shown.height &&
      (!state.expected || (state.expected.frame_index === state.shown.index && state.expected.sha256 === state.shown.hash));
  }
  function displayState() {
    const matched = matchesFrame(), shown = state.shown;
    $("display").textContent = shown ? `Displayed frame ${shown.index}${matched ? " · inspector synchronized" : " · waiting for synchronized inspector / stream"}` : "Waiting for the first frame…";
    $("display").dataset.frame = shown ? String(shown.index) : "";
    $("display").dataset.synchronized = String(Boolean(matched));
    $("display").dataset.sha256 = shown?.hash || "";
    $("display").dataset.publication = shown ? String(shown.publication) : "";
    $("details").hidden = !matched;
    drawOverlay();
  }
  function layers() {
    return [...$("layers").querySelectorAll("input:checked")].reduce((mask, box) => mask | Number(box.value), 0);
  }
  async function refresh() {
    const snapshot = await api("/api/inspect"), v = viewOf(snapshot);
    const changedCapture = state.snapshot && (state.snapshot.view.frame_index !== v.frame_index ||
      JSON.stringify(state.snapshot.nodes.map(n => [n.id,n.root,n.parents,n.children])) !== JSON.stringify(snapshot.nodes.map(n => [n.id,n.root,n.parents,n.children])));
    const mask = layers();
    const overlay = mask ? await api("/api/overlays?layers=" + mask) : null;
    if (overlay && (overlay.version !== 1 || overlay.layers !== mask || !Array.isArray(overlay.nodes) || !Array.isArray(overlay.tiles))) {
      throw new Error("Worker returned unsupported overlay data.");
    }
    if (changedCapture) { state.selected = null; state.collapsed.clear(); }
    state.snapshot = snapshot; state.overlay = overlay;
    for (const id of ["timeline", "frame"]) {
      $(id).max = String(v.frame_count - 1);
      if (!state.pending) $(id).value = String(v.frame_index);
    }
    $("timeline").disabled = false;
    $("position").textContent = `${v.frame_index} / ${v.frame_count - 1} · ${snapshot.scene_time.toFixed(3)} s · ${v.fps} fps`;
    $("input-events").disabled = !v.input_events;
    if (!v.input_events) $("input-events").checked = false;
    $("input-support").textContent = v.input_events ? "Enable to route pointer and keyboard events from the focused preview." : "This native preview has no live scene input adapter. Timeline and inspector controls remain available.";
    if (!snapshot.nodes.some(node => node.id === state.selected)) state.selected = null;
    renderTree(); renderDetails(); displayState();
  }
  function textElement(tag, text) { const e = document.createElement(tag); e.textContent = text; return e; }
  function focusTreeEntry(entry) {
    const target = $("tree").querySelector(`[data-entry="${entry}"]`);
    if (!target) return;
    for (const item of $("tree").querySelectorAll("[role=treeitem]")) item.tabIndex = item === target ? 0 : -1;
    target.focus();
  }
  function renderTree() {
    const tree = $("tree"), nodes = new Map(state.snapshot.nodes.map(n => [n.id, n]));
    const focused = document.activeElement?.dataset.entry;
    tree.replaceChildren();
    let count = 0;
    let tabAssigned = false;
    // Iterative traversal bounds both DAG expansion and call-stack use. IDs are
    // capture-local; restart clears selection and expansion state explicitly.
    const stack = state.snapshot.nodes.filter(n => n.root).reverse().map(node => ({node, parent: tree, depth: 1, ancestors: new Set()}));
    while (stack.length && count < 1000) {
      const {node, parent, depth, ancestors} = stack.pop();
      if (ancestors.has(node.id) || depth > 64) continue;
      count++;
      const item = document.createElement("div");
      item.role = "treeitem"; item.dataset.node = String(node.id); item.dataset.entry = String(count);
      const tabStop = !tabAssigned && (state.selected === null || node.id === state.selected);
      item.tabIndex = tabStop ? 0 : -1; tabAssigned ||= tabStop;
      item.setAttribute("aria-level", String(depth)); item.setAttribute("aria-selected", String(node.id === state.selected));
      const children = node.children.map(id => nodes.get(id)).filter(Boolean);
      const expanded = !state.collapsed.has(node.id);
      if (children.length) item.setAttribute("aria-expanded", String(expanded));
      const label = `${children.length ? (expanded ? "▾ " : "▸ ") : "· "}#${node.id}${node.source_span ? " " + node.source_span.kind : " Object"} · ${node.records.count} records`;
      const caption = textElement("span", label); caption.className = "tree-label"; item.append(caption); parent.append(item);
      item.addEventListener("click", event => {
        event.stopPropagation(); state.selected = node.id;
        if (children.length) { if (expanded) state.collapsed.add(node.id); else state.collapsed.delete(node.id); }
        renderTree(); renderDetails(); focusTreeEntry(item.dataset.entry); drawOverlay();
      });
      if (children.length && expanded) {
        const group = document.createElement("div"); group.role = "group"; item.append(group);
        const next = new Set(ancestors); next.add(node.id);
        for (const child of children.reverse()) stack.push({node: child, parent: group, depth: depth + 1, ancestors: next});
      }
    }
    if (!tabAssigned && tree.firstElementChild) tree.firstElementChild.tabIndex = 0;
    $("tree-state").textContent = `${state.snapshot.nodes.length} captured objects · ${count} visible entries · selection belongs to this frame${state.snapshot.truncated || stack.length ? " · display truncated; expand fewer branches" : ""}`;
    if (focused) focusTreeEntry(focused);
  }
  $("tree").addEventListener("keydown", event => {
    const item = event.target.closest("[role=treeitem]"); if (!item) return;
    const items = [...$("tree").querySelectorAll("[role=treeitem]")], index = items.indexOf(item);
    let target;
    switch (event.key) {
      case "ArrowDown": target = items[Math.min(index + 1, items.length - 1)]; break;
      case "ArrowUp": target = items[Math.max(index - 1, 0)]; break;
      case "Home": target = items[0]; break;
      case "End": target = items.at(-1); break;
      case "ArrowRight":
        if (item.getAttribute("aria-expanded") === "false") { state.collapsed.delete(Number(item.dataset.node)); renderTree(); }
        else target = item.querySelector("[role=treeitem]"); break;
      case "ArrowLeft":
        if (item.getAttribute("aria-expanded") === "true") { state.collapsed.add(Number(item.dataset.node)); renderTree(); }
        else target = item.parentElement.closest("[role=treeitem]"); break;
      case "Enter": case " ": target = item; break;
      default: return;
    }
    event.preventDefault();
    if (target) { state.selected = Number(target.dataset.node); renderTree(); renderDetails();
      focusTreeEntry(target.dataset.entry); drawOverlay(); }
  });
  function renderDetails() {
    const box = $("details"); box.replaceChildren();
    const node = state.snapshot.nodes.find(n => n.id === state.selected);
    $("selection").textContent = node ? `Object #${node.id} · frame ${state.snapshot.view.frame_index}` : "Select an object in this frame";
    if (!node) return;
    const info = document.createElement("dl");
    for (const [key, value] of Object.entries({Frame: state.snapshot.view.frame_index, Parents: node.parents.join(", ") || "root",
      Children: node.children.join(", ") || "none", "Z index": node.z_index, Animating: node.animating, Changing: node.changing})) {
      info.append(textElement("dt", key), textElement("dd", String(value)));
    }
    box.append(info);
    function section(title, value, open = false) {
      const detail = document.createElement("details"); detail.open = open;
      detail.append(textElement("summary", title), textElement("pre", typeof value === "string" ? value : JSON.stringify(value, null, 2))); box.append(detail);
    }
    if (node.source_span) {
      const s = node.source_span;
      section("Source span", `${s.kind} · UTF-8 bytes ${s.start}..${s.end} of ${s.source_bytes}\n${s.excerpt}${s.excerpt_truncated ? "\n[excerpt truncated]" : ""}`, true);
    } else section("Source span", "No source span for this object.");
    for (const field of node.records.fields.slice(0, 128)) section(`Record: ${field.name} · width ${field.width} · revision ${field.revision}`,
      {values: field.values, total_values: field.total_values, truncated: field.values.length < field.total_values}, field.name === "point");
    section("Uniforms", node.uniforms); section("Placement", node.placement); section("Bounds", node.bounds);
  }
  function drawOverlay() {
    const canvas = $("overlay"), ctx = canvas.getContext("2d"); ctx.clearRect(0, 0, canvas.width, canvas.height);
    const data = state.overlay, mask = layers();
    if (!mask) { $("overlay-state").textContent = "Overlays off."; return; }
    if (!data || data.layers !== mask || !matchesFrame()) { $("overlay-state").textContent = "Waiting for overlay data matching the displayed frame…"; return; }
    const v = state.snapshot.view;
    const point = p => Array.isArray(p) && p.length >= 2 && p.slice(0, 2).every(Number.isFinite) ? [v.origin[0] + p[0] * v.scale, v.origin[1] + p[1] * v.scale] : null;
    ctx.lineWidth = Math.max(1, v.width / 960); ctx.font = `${Math.max(10, v.width / 80)}px monospace`;
    let primitives = 0;
    const inspectedNodes = new Map(state.snapshot.nodes.map(node => [node.id, node]));
    if (mask & 1) { ctx.strokeStyle = "#71def560"; for (const tile of data.tiles.slice(0, 10000)) {
      if (!tile.rect.every(Number.isFinite)) continue;
      const [x0,y0,x1,y1] = tile.rect; ctx.strokeRect(x0,y0,x1-x0,y1-y0); primitives++;
    } }
    for (const node of data.nodes.slice(0, 1000)) {
      ctx.strokeStyle = node.id === state.selected ? "#fff59d" : "#6ff5b3b0"; ctx.fillStyle = ctx.strokeStyle;
      if (primitives >= 20000) break;
      if (mask & 2) for (const p of node.control_points.slice(0, Math.min(4096, 20000 - primitives))) { const q = point(p); if (q) { ctx.beginPath(); ctx.arc(q[0],q[1],Math.max(0.5,v.width/960),0,2*Math.PI); ctx.fill(); primitives++; } }
      const a = node.bounds && point(node.bounds[0]), b = node.bounds && point(node.bounds[2]);
      if (a && b && (mask & 4)) { ctx.strokeRect(a[0],a[1],b[0]-a[0],b[1]-a[1]); primitives++; }
      const inspected = inspectedNodes.get(node.id);
      const anchor = a || (inspected?.bounds && point(inspected.bounds[0])) || (node.control_points.length && point(node.control_points[0]));
      if (anchor && node.id === state.selected && (mask & 24)) {
        const winding = {counter_clockwise:"CCW",clockwise:"CW",degenerate:"degenerate"}[node.winding] || node.winding;
        const label = [`#${node.id}`, mask & 8 ? winding : null, mask & 16 ? `z=${node.center_z}` : null].filter(x => x !== null).join(" · ");
        const x = Math.max(0,Math.min(v.width-ctx.measureText(label).width,anchor[0])), y = Math.max(12,anchor[1]-4);
        ctx.fillText(label,x,y); primitives++;
      }
    }
    const selected = data.nodes.find(node => node.id === state.selected);
    const readout = mask & 24 ? (selected ? ` · #${selected.id}: ${mask & 8 ? selected.winding : ""}${mask & 16 ? ` z=${selected.center_z}, order=${selected.z_index}, depth=${selected.depth_test}` : ""}` : " · select an object for winding / depth labels") : "";
    $("overlay-state").textContent = `${primitives} overlay marks · frame ${v.frame_index}${readout}${data.truncated || primitives >= 20000 || data.tiles.length > 10000 || data.nodes.length > 1000 ? " · bounded display truncated" : ""}`;
    $("overlay-state").dataset.marks = String(primitives);
  }
  async function run(operation) {
    if (state.busy) return;
    state.busy = true; $("restart").disabled = true; $("inspect").disabled = true;
    try { clearError(); await operation(); }
    catch (error) { state.playing = false; report(error); }
    finally {
      state.busy = false; $("restart").disabled = false; $("inspect").disabled = false; $("play").textContent = state.playing ? "Pause" : "Play";
      if (state.pending) void drain();
      else if (state.refreshNeeded) { state.refreshNeeded = false; void run(refresh); }
    }
  }
  async function drain() {
    await run(async () => {
      while (state.pending) {
        const {frame, commit} = state.pending; state.pending = null;
        state.overlay = null; displayState();
        state.expected = await api("/api/scrub", {frame: String(frame), commit: String(commit)});
        await refresh();
        $("replay").textContent = commit ? `Committed frame ${frame} for worker replay.` : `Previewing frame ${frame}. Release the timeline to commit.`;
      }
    });
  }
  function seek(frame, commit = true) {
    const v = state.snapshot?.view;
    if (!v || !Number.isSafeInteger(frame) || frame < 0 || frame >= v.frame_count) { report(new Error(`Choose an integer frame from 0 to ${v ? v.frame_count - 1 : "the available range"}.`)); return; }
    state.pending = {frame, commit}; void drain();
  }
  $("timeline").addEventListener("input", () => { state.playing = false; seek(Number($("timeline").value), false); });
  $("timeline").addEventListener("change", () => seek(Number($("timeline").value), true));
  $("timeline-form").addEventListener("submit", event => { event.preventDefault(); state.playing = false; seek(Number($("frame").value)); });
  for (const [id, next] of [["first", () => 0], ["last", () => state.snapshot.view.frame_count - 1],
    ["previous", () => Math.max(0, state.snapshot.view.frame_index - 1)], ["next", () => Math.min(state.snapshot.view.frame_count - 1, state.snapshot.view.frame_index + 1)]]) {
    $(id).addEventListener("click", () => { state.playing = false; if (state.snapshot) seek(next()); });
  }
  $("play").addEventListener("click", () => { state.playing = !state.playing; $("play").textContent = state.playing ? "Pause" : "Play"; });
  // Preview playback advances nominal frames; it never invents variable sampling
  // or claims real-time rendering when the worker takes longer than a frame.
  let lastTick = 0;
  function tick(now) {
    const v = state.snapshot?.view;
    if (state.playing && v && !state.busy && now - lastTick >= 1000 / v.fps) {
      lastTick = now;
      if (v.frame_index + 1 < v.frame_count) seek(v.frame_index + 1, false);
      else { state.playing = false; $("play").textContent = "Play"; seek(v.frame_index, true); }
    }
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
  $("inspect").addEventListener("click", () => void run(refresh));
  $("layers").addEventListener("change", () => { if (!state.busy) void run(refresh); else state.refreshNeeded = true; });
  $("restart").addEventListener("click", () => void run(async () => {
    state.playing = false; state.pending = null; state.overlay = null; state.snapshot = null; displayState();
    $("worker").textContent = "Restarting worker and replaying committed state…";
    const result = await api("/api/restart", {});
    state.expected = result; state.selected = null; state.collapsed.clear();
    await refresh();
    $("worker").textContent = `Worker generation ${result.worker_generation} ready`;
    $("replay").textContent = `Restart restored frame ${result.frame_index} · ${result.reused_entries} reused, ${result.replayed_entries} replayed, ${result.reexecuted_entries} re-executed entries${result.cold_fallback ? " · cold fallback" : ""}.`;
  }));
  const keyNames = {ArrowLeft:"arrow_left", ArrowRight:"arrow_right", ArrowUp:"arrow_up", ArrowDown:"arrow_down", Escape:"escape", Enter:"enter", Tab:"tab", Backspace:"backspace"};
  function routeInput(event, fields) {
    if (!$("input-events").checked || !state.snapshot?.view.input_events) return;
    event.preventDefault();
    if (state.busy) { report(new Error("Scene input is busy; retry after the current request completes.")); return; }
    const modifiers = String(Number(event.shiftKey) | Number(event.ctrlKey) << 1 | Number(event.metaKey) << 2 | Number(event.altKey) << 3);
    void run(async () => { await api("/api/event", {...fields, modifiers}); await refresh(); });
  }
  for (const type of ["keydown", "keyup"]) $("preview").addEventListener(type, event => {
    const key = keyNames[event.key] || ([...event.key].length === 1 ? event.key : null);
    if (key) routeInput(event, {type: type === "keydown" ? "key_press" : "key_release", key});
  });
  for (const type of ["pointerdown", "pointerup", "wheel"]) $("preview").addEventListener(type, event => {
    const v = state.snapshot?.view; if (!v) return;
    const rect = $("preview").getBoundingClientRect();
    const x = ((event.clientX - rect.left) * v.width / rect.width - v.origin[0]) / v.scale;
    const y = ((event.clientY - rect.top) * v.height / rect.height - v.origin[1]) / v.scale;
    if (type === "pointerdown") $("preview").focus();
    routeInput(event, type === "wheel" ? {type:"mouse_scroll", x, y, offset_x:event.deltaX, offset_y:event.deltaY} :
      {type: type === "pointerdown" ? "mouse_press" : "mouse_release", x, y, button: ["left","middle","right"][event.button] || `other:${event.button}`});
  }, {passive: false});
  // FrameHub parts carry a length, frame index and digest. Decode only a bounded
  // complete part; image arrival is independent from the inspector HTTP request.
  async function streamOnce() {
    const controller = new AbortController();
    let watchdog = setTimeout(() => controller.abort(), 45000);
    let reader;
    try {
    const response = await fetch("/stream", {headers, signal: controller.signal});
    if (!response.ok) throw new Error(`Preview stream refused (${response.status}). Restart Studio if the session expired.`);
    if (response.headers.get("Content-Type") !== "multipart/x-mixed-replace; boundary=fmn-frame") throw new Error("Unsupported preview stream format.");
    reader = response.body.getReader();
    let chunk = new Uint8Array();
    let offset = 0;
    let lastPublication = -1;
    async function byte() {
      if (offset === chunk.length) {
        const next = await reader.read(); if (next.done) return null;
        chunk = next.value; offset = 0;
        if (!chunk.length) return byte();
      }
      return chunk[offset++];
    }
    async function line() {
      const bytes = [];
      while (bytes.length <= 1024) { const b = await byte(); if (b === null) return null; bytes.push(b); if (b === 10) return new TextDecoder().decode(new Uint8Array(bytes)).trim(); }
      throw new Error("Preview stream header exceeds budget.");
    }
      for (;;) {
        let boundary = await line();
        while (boundary === "") boundary = await line();
        if (boundary === null || boundary === "--fmn-frame--") break;
        if (boundary !== "--fmn-frame") throw new Error("Invalid preview stream boundary.");
        const fields = new Map(); let ended = false;
        for (let i = 0; i < 12; i++) {
          const field = await line(); if (field === null) throw new Error("Preview stream ended inside a header.");
          if (!field) { ended = true; break; }
          const colon = field.indexOf(":"); if (colon < 1) throw new Error("Invalid preview header.");
          const name = field.slice(0,colon).toLowerCase();
          if (fields.has(name)) throw new Error("Duplicate preview header.");
          fields.set(name, field.slice(colon+1).trim());
        }
        const length = Number(fields.get("content-length")), index = Number(fields.get("x-fmn-frame-index")), hash = fields.get("x-fmn-sha256");
        const publication = Number(fields.get("x-fmn-publication-sequence"));
        if (!ended || fields.get("content-type") !== "image/png" || !Number.isSafeInteger(publication) || publication <= lastPublication ||
          !Number.isSafeInteger(length) || length < 24 || length > MAX_PNG || !Number.isSafeInteger(index) || index < 0 || !/^[a-f0-9]{64}$/.test(hash)) throw new Error("Invalid preview frame metadata or payload budget.");
        const bytes = new Uint8Array(length); let at = 0;
        while (at < length) {
          if (offset === chunk.length) { const next = await reader.read(); if (next.done) throw new Error("Incomplete preview frame."); chunk = next.value; offset = 0; }
          const n = Math.min(length-at,chunk.length-offset); bytes.set(chunk.subarray(offset,offset+n),at); offset += n; at += n;
        }
        const png = new DataView(bytes.buffer), width = png.getUint32(16), height = png.getUint32(20);
        if (png.getUint32(0) !== 0x89504e47 || png.getUint32(4) !== 0x0d0a1a0a) throw new Error("Preview payload is not PNG.");
        if (width < 1 || height < 1 || width * height > 33554432) throw new Error("Preview dimensions exceed the browser budget.");
        const digest = [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map(n => n.toString(16).padStart(2,"0")).join("");
        // ubs:ignore — public PNG content hashes do not authenticate a bearer token.
        if (digest !== hash) throw new Error("Preview frame digest mismatch.");
        const bitmap = await createImageBitmap(new Blob([bytes], {type:"image/png"}));
        try {
          for (const id of ["preview", "overlay"]) { $(id).width = width; $(id).height = height; }
          $("preview").getContext("2d").drawImage(bitmap,0,0);
        } finally { bitmap.close(); }
        lastPublication = publication;
        state.connected = true; state.shown = {index,hash,width,height,publication}; displayState();
        clearTimeout(watchdog); watchdog = setTimeout(() => controller.abort(), 45000);
      }
    } finally { clearTimeout(watchdog); await reader?.cancel().catch(() => {}); }
  }
  async function stream() {
    let failures = 0;
    while (failures < 5) {
      try { await streamOnce(); failures = 0; }
      catch (error) { failures++; report(error); }
      state.connected = false; displayState();
      await new Promise(resolve => setTimeout(resolve, Math.min(1000 * 2 ** failures, 16000)));
    }
    $("worker").textContent = "Preview disconnected. Reopen the Studio launch URL to reconnect.";
  }
  void stream();
  void run(async () => { await refresh(); $("worker").textContent = "Worker ready"; });
})();
