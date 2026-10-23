// The browser worker transports validated native output, never scene code.
export const WORKER_PROTOCOL = 'fmn.picture-worker.v2';
export const MAX_BUNDLE_BYTES = 256 * 1024 * 1024;
export const MAX_PCM_BYTES = 256 * 1024 * 1024;
export const COPY_FRAMES = 65_536;
const MAX_U32 = 0xffff_ffff;

export function integer(value, name, minimum = 0, maximum = MAX_U32) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new RangeError(`Invalid ${name}.`);
  }
  return value;
}
export function viewport(width, height) {
  integer(width, 'viewport width', 1, 4096);
  integer(height, 'viewport height', 1, 4096);
  return width * height * 4;
}
export function audioInfo(value, limit = MAX_PCM_BYTES) {
  integer(limit, 'PCM byte limit', 1, MAX_PCM_BYTES);
  const sampleRate = integer(value?.sampleRate, 'sample rate', 1);
  const channels = integer(value?.channels, 'channel count', 1, 2);
  const sampleFrames = integer(value?.sampleFrames, 'sample count');
  if (sampleFrames * channels * 4 > limit) throw new RangeError('Soundtrack exceeds the PCM byte budget.');
  return Object.freeze({sampleRate, channels, sampleFrames});
}
export function metadata(value) {
  const frameCount = integer(value?.frameCount, 'frame count', 1);
  const fps = integer(value?.fps, 'frame rate', 1);
  if (!Number.isFinite(value.durationSeconds) || value.durationSeconds < 0 ||
      typeof value.engineVersion !== 'string' || !value.engineVersion.length || value.engineVersion.length > 4096 ||
      typeof value.hasCameraTrack !== 'boolean' || typeof value.hasSoundtrack !== 'boolean' ||
      !Array.isArray(value.labels) || value.labels.length > 4096) throw new Error('Invalid recording metadata.');
  let bytes = 0;
  const labels = value.labels.map(label => {
    if (typeof label?.name !== 'string' || label.name.length > 4096) throw new Error('Invalid recording label.');
    bytes += new TextEncoder().encode(label.name).byteLength;
    if (bytes > 1024 * 1024) throw new RangeError('Recording labels exceed the metadata budget.');
    if (label.index !== null) integer(label.index, 'label frame', 0, frameCount - 1);
    return Object.freeze({name: label.name, index: label.index});
  });
  const audio = value.hasSoundtrack ? audioInfo(value.audio) : null;
  if (!value.hasSoundtrack && value.audio !== null) throw new Error('Unexpected soundtrack metadata.');
  return Object.freeze({frameCount, fps, durationSeconds: value.durationSeconds,
    engineVersion: value.engineVersion, hasCameraTrack: value.hasCameraTrack,
    hasSoundtrack: value.hasSoundtrack, audio, labels: Object.freeze(labels)});
}
export function pixels(buffer, width, height) {
  if (!(buffer instanceof ArrayBuffer) || buffer.byteLength !== viewport(width, height)) {
    throw new Error('Invalid transferred picture buffer.');
  }
  return buffer;
}
export function channelBuffers(buffers, info) {
  if (!Array.isArray(buffers) || buffers.length !== info.channels || new Set(buffers).size !== buffers.length ||
      buffers.some(buffer => !(buffer instanceof ArrayBuffer) || buffer.byteLength !== info.sampleFrames * 4)) {
    throw new Error('Invalid transferred soundtrack buffers.');
  }
  return buffers;
}

// fmn-hash canonical envelopes end in SHA-256(header + payload). This is
// only an identity after the native FMTL decoder has verified the envelope;
// reading its trailer never substitutes for native integrity/schema admission.
export function recordingSource(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength < 56 || bytes.byteLength > MAX_BUNDLE_BYTES) {
    throw new RangeError('Invalid framed recording bytes.');
  }
  const checksum = [...bytes.subarray(bytes.length - 32)].map(n => n.toString(16).padStart(2, '0')).join('');
  return sourceIdentity({byteLength: bytes.byteLength, checksum});
}
export function sourceIdentity(value) {
  integer(value?.byteLength, 'recording byte length', 56, MAX_BUNDLE_BYTES);
  if (typeof value.checksum !== 'string' || !/^[0-9a-f]{64}$/.test(value.checksum)) {
    throw new Error('Invalid recording checksum identity.');
  }
  return Object.freeze({byteLength: value.byteLength, checksum: value.checksum});
}
export function sameSource(left, right) {
  return left.byteLength === right.byteLength && left.checksum === right.checksum;
}

// One request owns at most 512 KiB of stereo float32 data, independent of the
// recording length. Both endpoints check the interval before native allocation.
export function audioWindow(start, count, info) {
  integer(start, 'audio window start', 0, info.sampleFrames);
  integer(count, 'audio window length', 1, COPY_FRAMES);
  if (count > info.sampleFrames - start) throw new RangeError('Audio window exceeds the soundtrack.');
}
export function windowChannels(buffers, start, count, info) {
  audioWindow(start, count, info);
  return channelBuffers(buffers, {...info, sampleFrames: count});
}
