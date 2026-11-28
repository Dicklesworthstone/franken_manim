// Fonts, layout and capture live in the optional authoring worker. Only the
// portable FMTL bytes leave it; the picture worker uses the ordinary player.
import init, {FmnScribe} from './pkg-scribe/fmn_wasm.js';
let scribe;
self.onmessage = async ({data}) => {
  let source, target;
  try {
    if (!scribe) {
      await init();
      scribe = new FmnScribe();
    }
    source = scribe.typeset(data.kind, data.from, data.fontSize);
    target = scribe.typeset(data.kind, data.to, data.fontSize);
    const bytes = source.transition_to(target, data.fps, data.seconds);
    self.postMessage({bytes}, [bytes.buffer]);
  } catch (error) {
    self.postMessage({error: String(error?.message || error)});
  } finally {
    target?.free();
    source?.free();
  }
};
