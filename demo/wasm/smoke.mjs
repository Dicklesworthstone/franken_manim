import { FmnPlayer, FmnScene, engine_version } from "fmn-wasm";

const result = document.getElementById("result");
if (result === null) {
  throw new Error("browser smoke result element is missing");
}

function check(condition, message) {
  if (!condition) throw new Error(message);
}

function equalBytes(left, right, message) {
  check(left.length === right.length, `${message}: length mismatch`);
  for (let i = 0; i < left.length; i += 1) {
    if (left[i] !== right[i]) {
      throw new Error(`${message}: byte ${i} differs (${left[i]} != ${right[i]})`);
    }
  }
}

function fnv1a(bytes) {
  let hash = 0x811c9dc5;
  for (const byte of bytes) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

function drawAndReadBack(canvas, bytes, width, height) {
  const context = canvas.getContext("2d", { willReadFrequently: true });
  check(context !== null, "2D canvas context unavailable");
  const clamped = new Uint8ClampedArray(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  context.putImageData(new ImageData(clamped, width, height), 0, 0);
  return context.getImageData(0, 0, width, height).data;
}

async function main() {
  const expectedVersion = new URL(window.location.href).searchParams.get("version");
  check(expectedVersion !== null, "missing expected package version");
  check(engine_version() === expectedVersion, "compiled engine/package version mismatch");

  const kinds = FmnScene.scene_kinds();
  check(
    JSON.stringify(kinds) === JSON.stringify(["circle_shift", "parametric_wave", "orbit_duet"]),
    `unexpected scene inventory: ${JSON.stringify(kinds)}`,
  );

  const scene = new FmnScene("circle_shift", 96, 54);
  try {
    check(scene.frame_count > 1, "primitive scene did not capture multiple frames");
    const scenePixels = new Uint8Array(scene.width * scene.height * 4);
    const sceneRepeat = new Uint8Array(scenePixels.length);
    scene.render_into(0, scenePixels);
    scene.render_into(0, sceneRepeat);
    equalBytes(scenePixels, sceneRepeat, "primitive render is not deterministic");

    let wrongLengthRefused = false;
    try {
      scene.render_into(0, new Uint8Array(scenePixels.length - 1));
    } catch (error) {
      wrongLengthRefused = String(error).includes("expected");
    }
    check(wrongLengthRefused, "wrong-length primitive destination did not refuse precisely");
    const sceneReadback = drawAndReadBack(
      document.getElementById("scene-canvas"),
      scenePixels,
      scene.width,
      scene.height,
    );
    equalBytes(scenePixels, sceneReadback, "primitive canvas readback");

    const response = await fetch("./bundle.fmtl", {
      signal: AbortSignal.timeout(10_000),
    });
    check(response.ok, `bundle fetch failed: HTTP ${response.status}`);
    const player = FmnPlayer.from_bundle(new Uint8Array(await response.arrayBuffer()));
    try {
      player.set_viewport(96, 54);
      check(player.frame_count > 1, "FMTL player has too few frames");
      check(player.labels().length > 0, "FMTL player lost authored labels");
      const finalIndex = player.frame_count - 1;
      player.seek_frame(finalIndex);
      check(player.current_frame === finalIndex, "FMTL seek did not update the cursor");
      const playerPixels = new Uint8Array(player.width * player.height * 4);
      const playerRepeat = new Uint8Array(playerPixels.length);
      player.render_into(finalIndex, playerPixels);
      player.render_into(finalIndex, playerRepeat);
      equalBytes(playerPixels, playerRepeat, "FMTL render is not deterministic");
      const playerReadback = drawAndReadBack(
        document.getElementById("player-canvas"),
        playerPixels,
        player.width,
        player.height,
      );
      equalBytes(playerPixels, playerReadback, "FMTL canvas readback");

      result.dataset.status = "success";
      result.textContent = JSON.stringify({
        status: "success",
        version: engine_version(),
        sceneDigest: fnv1a(scenePixels),
        sceneFrames: scene.frame_count,
        playerDigest: fnv1a(playerPixels),
        playerFrames: player.frame_count,
        playerEngine: player.engine_version,
      });
    } finally {
      player.free();
    }
  } finally {
    scene.free();
  }
}

main().catch((error) => {
  result.dataset.status = "failure";
  result.textContent = String(error?.stack ?? error);
});
