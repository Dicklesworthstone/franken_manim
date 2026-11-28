// Compile off the UI thread and reuse the player's normal load and recovery paths.
export function mountTransitionEditor(panel, player) {
  const form = panel.querySelector('form');
  const {kind, from, to, fontSize, fps, seconds} = form.elements;
  const build = panel.querySelector('[type=submit]');
  const cancel = panel.querySelector('[data-cancel]');
  const status = panel.querySelector('[role=status]');
  const download = panel.querySelector('[download]');
  let worker = null, downloadUrl = null, disposed = false;

  function finish(message) {
    status.textContent = message;
    build.disabled = false;
    cancel.disabled = true;
  }
  function discardWorker() {
    worker?.terminate();
    worker = null;
  }
  function fail(error) {
    discardWorker();
    finish('Could not build the transition. ' + error);
  }
  function openFromLink() {
    if (location.hash === '#compose') panel.open = true;
  }
  openFromLink();
  window.addEventListener('hashchange', openFromLink);
  form.addEventListener('submit', event => {
    event.preventDefault();
    if (build.disabled || disposed) return;
    build.disabled = true;
    cancel.disabled = false;
    status.textContent = 'Typesetting and recording…';
    try {
      if (!worker) {
        const active = new Worker(new URL('./transition-worker.mjs', import.meta.url), {type: 'module'});
        worker = active;
        active.onerror = event => {
          if (worker !== active) return;
          event.preventDefault();
          fail(event.message || 'The optional Scribe module could not be loaded.');
        };
        active.onmessageerror = () => { if (worker === active) fail('Invalid worker response.'); };
        active.onmessage = async ({data}) => {
          if (worker !== active || disposed) return;
          if (data.error) { finish(data.error); return; }
          cancel.disabled = true;
          status.textContent = 'Opening the compiled recording…';
          try {
            const blob = new Blob([data.bytes], {type: 'application/octet-stream'});
            const loaded = await player.load(async () => new Uint8Array(await blob.arrayBuffer()), 'Authored transition');
            if (disposed || worker !== active) return;
            if (!loaded) { finish('The transition was not opened.'); return; }
            const url = URL.createObjectURL(blob);
            if (downloadUrl) URL.revokeObjectURL(downloadUrl);
            download.href = downloadUrl = url;
            download.hidden = false;
            finish('Ready to play or scrub.');
          } catch (error) {
            if (!disposed && worker === active) finish(String(error));
          }
        };
      }
      worker.postMessage({kind: kind.value, from: from.value, to: to.value,
        fontSize: fontSize.valueAsNumber, fps: fps.valueAsNumber, seconds: seconds.valueAsNumber});
    } catch (error) { fail(error); }
  });
  cancel.addEventListener('click', () => {
    discardWorker();
    finish('Build cancelled.');
  });
  window.addEventListener('pagehide', event => {
    if (event.persisted) return;
    disposed = true;
    discardWorker();
    if (downloadUrl) URL.revokeObjectURL(downloadUrl);
    window.removeEventListener('hashchange', openFromLink);
  });
}
