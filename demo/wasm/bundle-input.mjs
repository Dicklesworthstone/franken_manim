import {MAX_BUNDLE_BYTES, integer} from './worker-protocol.mjs';

// Bound ownership while reading, not after an unbounded arrayBuffer(). Cancel
// superseded reads even when their stream has not produced its next chunk.
export async function readBundleStream(stream, {signal, expectedSize = null, maxBytes = MAX_BUNDLE_BYTES} = {}) {
  if (!stream || typeof stream.getReader !== 'function') throw new Error('Recording has no readable byte stream.');
  const reader = stream.getReader();
  const abort = () => { void reader.cancel(signal.reason).catch(() => {}); };
  try {
    integer(maxBytes, 'bundle byte limit', 1, MAX_BUNDLE_BYTES);
    if (expectedSize !== null) integer(expectedSize, 'declared bundle size', 0, maxBytes);
    signal?.addEventListener('abort', abort, {once: true});
    signal?.throwIfAborted();
    const fixed = expectedSize === null ? null : new Uint8Array(expectedSize);
    const chunks = [];
    let total = 0, count = 0;
    for (;;) {
      const {done, value} = await reader.read();
      signal?.throwIfAborted();
      if (done) break;
      if (!(value instanceof Uint8Array) || ++count > 65_536) throw new Error('Invalid or excessive recording chunks.');
      if (value.byteLength > maxBytes - total || (fixed && value.byteLength > fixed.length - total)) {
        throw new RangeError('Recording exceeds its byte budget or declared size.');
      }
      if (fixed) fixed.set(value, total);
      else if (value.length) chunks.push(value.slice());
      total += value.byteLength;
    }
    if (fixed) {
      if (total !== fixed.length) throw new Error('Recording ended before its declared size.');
      return fixed;
    }
    const result = new Uint8Array(total);
    let offset = 0;
    for (const chunk of chunks) { result.set(chunk, offset); offset += chunk.length; }
    return result;
  } finally {
    signal?.removeEventListener('abort', abort);
    void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export function readBundleFile(file, signal, maxBytes = MAX_BUNDLE_BYTES) {
  integer(maxBytes, 'bundle byte limit', 1, MAX_BUNDLE_BYTES);
  integer(file?.size, 'local recording size', 0, maxBytes);
  if (typeof file.stream !== 'function') throw new Error('This browser cannot stream local recording files.');
  return readBundleStream(file.stream(), {signal, expectedSize: file.size, maxBytes});
}

export async function readBundleResponse(response, signal, maxBytes = MAX_BUNDLE_BYTES) {
  try {
    if (!response.ok) throw new Error(`Recording request refused (${response.status}).`);
    const length = response.headers.get('Content-Length');
    if (length !== null && !/^(0|[1-9][0-9]*)$/.test(length)) throw new Error('Invalid recording Content-Length.');
    return await readBundleStream(response.body, {signal,
      expectedSize: length === null ? null : Number(length), maxBytes});
  } catch (error) {
    if (response.body && !response.body.locked) void response.body.cancel().catch(() => {});
    throw error;
  }
}
