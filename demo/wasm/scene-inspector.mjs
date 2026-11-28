import {INSPECT_PAGE_SIZE} from './worker-protocol.mjs';

// Inspection is explicit. Playback and scrubbing never reconstruct a second
// stage just to update a hidden panel. A returned page belongs to one display.
export function mountSceneInspector(document, section, stop) {
  const inspect = section.querySelector('[data-inspect]');
  const caption = section.querySelector('[data-caption]');
  const list = section.querySelector('ul');
  const nav = section.querySelector('nav');
  let owner = null, frame = null, available = false, revision = 0, busy = false;
  let parent = null, offset = 0, history = [];

  function clear() {
    ++revision; busy = false; parent = null; offset = 0; history = [];
    list.replaceChildren(); nav.replaceChildren(); caption.textContent = '';
  }
  function update(renderer, index, ready) {
    if (owner !== renderer || frame !== index || available !== ready) clear();
    owner = renderer; frame = index; available = ready;
    inspect.disabled = !available || busy;
  }
  function button(container, label, action) {
    const element = document.createElement('button');
    element.type = 'button'; element.textContent = label;
    element.addEventListener('click', action);
    container.appendChild(element);
  }
  async function show(nextParent, nextOffset, nextHistory) {
    if (!available || busy) return;
    stop();
    if (!available) return;
    const request = ++revision;
    busy = true; inspect.disabled = true;
    caption.textContent = 'Reading recorded objects…';
    list.replaceChildren(); nav.replaceChildren();
    try {
      const page = await owner.inspect(frame, nextParent, nextOffset);
      if (request !== revision) return;
      parent = nextParent; offset = nextOffset; history = nextHistory;
      const scope = parent ? `children of #${parent.slot}` : 'scene roots';
      const end = offset + page.objects.length;
      const count = page.total === 0 ? '0 objects' : `${offset + 1}–${end} of ${page.total}`;
      caption.textContent = `Frame ${frame} · ${scope} · ${count}`;
      for (const object of page.objects) {
        const li = document.createElement('li');
        li.textContent = `#${object.slot} ${object.kind} · ${object.points} points · z ${object.z} `;
        if (object.children) button(li, `Open children (${object.children})`, () =>
          show(object, 0, [...history, {parent, offset}]));
        list.appendChild(li);
      }
      if (history.length) button(nav, 'Up one level', () => {
        const previous = history[history.length - 1];
        show(previous.parent, previous.offset, history.slice(0, -1));
      });
      if (offset > 0) button(nav, 'Previous objects', () =>
        show(parent, Math.max(0, offset - INSPECT_PAGE_SIZE), history));
      if (end < page.total) button(nav, 'Next objects', () => show(parent, end, history));
    } catch (error) {
      if (request === revision) caption.textContent = `Inspection failed: ${error.message || error}`;
    } finally {
      if (request === revision) { busy = false; inspect.disabled = !available; }
    }
  }
  inspect.addEventListener('click', () => show(null, 0, []));
  return {update};
}
