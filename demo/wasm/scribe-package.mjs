// Run in Node, or import this module from the served demo in a browser.
// Both packages are independent wasm-bindgen web-target builds.
import initScribe, { FmnScribe } from "./pkg-scribe/fmn_wasm.js";
import initPlayer, * as player from "./pkg/fmn_wasm.js";

async function load(init, relativePath) {
  if (typeof window !== "undefined") {
    return init();
  }
  const { readFile } = await import("node:fs/promises");
  return init({ module_or_path: await readFile(new URL(relativePath, import.meta.url)) });
}

await load(initScribe, "./pkg-scribe/fmn_wasm_bg.wasm");
await load(initPlayer, "./pkg/fmn_wasm_bg.wasm");
if ("FmnScribe" in player) throw new Error("Expected the default player package");

// Native FmnScribe renders at font size 72 and a 240 × 128 viewport.
const cases = [
  ["text", "Velocity", "e8a8b1929d4baff7b6a1d2678d81cfcf8fa308dc9858c1f7d7745f6431617551"],
  ["markup", "<b>bold</b>", "4ffca0e7ceb91da1f1317976445cf21b8bb9328d2eb6c453e537c245562c6fd6"],
  ["math", String.raw`\frac{x^2}{2}`, "c87080fcf171abffe6815437bf7cdebe9b8b29d15a455f8999ed521b85484fc6"],
];
const scribe = new FmnScribe();
try {
  for (const [kind, source, expected] of cases) {
    const item = scribe.typeset(kind, source, 72);
    let replay;
    try {
      const rgba = new Uint8Array(240 * 128 * 4);
      item.render_into(240, 128, rgba);
      const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", rgba));
      const hex = Array.from(digest, value => value.toString(16).padStart(2, "0")).join("");
      if (hex !== expected) throw new Error(`${kind} differs from native RGBA: ${hex}`);

      replay = player.FmnPlayer.from_bundle(item.to_bundle());
      replay.set_viewport(240, 128);
      const pixels = replay.render_frame(0);
      if (replay.frame_count !== 1 || pixels.length !== rgba.length ||
          !pixels.every((value, index) => value === rgba[index])) {
        throw new Error(`${kind} changed during FMTL playback`);
      }
    } finally {
      replay?.free();
      item.free();
    }
  }

  const item = scribe.typeset("textext", String.raw`Rate $\pi$ Hz`, 72);
  let coloredReplay;
  try {
    if (item.select_range(7, 9).length || item.color_range(6, 9, "#0088ff") !== 1) {
      throw new Error("Partial TeX commands must preserve whole glyph ownership");
    }
    const frame = item.render(240, 128);
    if (!frame.some((v, i) => i % 4 === 2 && v > frame[i - 2])) {
      throw new Error("The selected inline math did not change color");
    }
    coloredReplay = player.FmnPlayer.from_bundle(item.to_bundle());
    coloredReplay.set_viewport(240, 128);
    const replay = coloredReplay.render_frame(0);
    if (frame.length !== replay.length || !frame.every((v, i) => v === replay[i])) {
      throw new Error("Inline math selection changed during FMTL playback");
    }
  } finally {
    coloredReplay?.free();
    item.free();
  }

  const unicode = scribe.typeset("markup", "<b>é&amp;𝒜</b>!", 72);
  try {
    const trailing = unicode.select_range(15, 16);
    if (trailing.length !== 1 || trailing[0] !== 3) {
      throw new Error("Textarea offsets after non-BMP text selected the wrong part");
    }
    for (const [start, end, color] of [[9, 10, "#f00"], [10, 9, "#f00"], [9, 11, "bad"]]) {
      let rejected = false;
      try { unicode.color_range(start, end, color); } catch { rejected = true; }
      if (!rejected) throw new Error("Accepted an invalid source selection or color");
    }
  } finally {
    unicode.free();
  }
} finally {
  scribe.free();
}
console.log("Scribe: native pixels, source selection, and styled FMTL replay passed.");
