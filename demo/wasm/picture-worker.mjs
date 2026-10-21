import {PictureWorkerRuntime} from './picture-worker-core.mjs';

// Import and instantiate inside the worker: even decoding and the first render
// must not run on the UI/audio scheduling thread. No user-selected code URL.
const runtime = new PictureWorkerRuntime(async bytes => {
  const {default: init, FmnPlayer} = await import('./pkg/fmn_wasm.js');
  await init();
  return FmnPlayer.from_bundle_with_audio(bytes);
}, (message, transfer) => self.postMessage(message, transfer));
self.onmessage = event => { void runtime.receive(event.data); };
self.onmessageerror = () => { runtime.dispose(); self.close(); };
