// Small DOM helpers shared by the lobby modules. Nothing in here knows about
// the game — it is all "find an element, wire it up, keep localStorage in sync".

export function el(id) {
  return document.getElementById(id);
}

export function esc(str) {
  return String(str ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export function onClick(id, handler) {
  el(id).addEventListener('click', handler);
}

// Checkbox backed by a localStorage flag. `defaultOn` decides which of the two
// stored values is treated as the fallback: a default-on toggle is checked
// unless the stored value is exactly `off`, a default-off one only when the
// stored value is exactly `on`.
export function bindToggle(id, key, { on = '1', off = '0', defaultOn = false, onChange } = {}) {
  const input = el(id);
  const stored = localStorage.getItem(key);
  input.checked = defaultOn ? stored !== off : stored === on;
  input.addEventListener('change', () => {
    localStorage.setItem(key, input.checked ? on : off);
    onChange?.(input.checked);
  });
  return input;
}

// Volume slider mirrored into localStorage, a numeric readout, and whatever
// audio engine happens to be alive (there is none until a match starts).
export function bindVolumeSlider(sliderId, readoutId, key, fallback, setVolume) {
  const slider = el(sliderId);
  const readout = el(readoutId);
  const saved = parseFloat(localStorage.getItem(key));
  const initial = Number.isFinite(saved) ? Math.max(0, Math.min(1, saved)) : fallback;
  slider.value = Math.round(initial * 100);
  readout.textContent = slider.value;
  slider.addEventListener('input', () => {
    const volume = slider.value / 100;
    readout.textContent = slider.value;
    localStorage.setItem(key, volume.toFixed(3));
    setVolume(volume);
  });
}

// A one-line status message that clears itself after `ms`. Passing ms = 0
// leaves the message up and leaves any pending clear alone — that is what the
// in-flight "Saving…" / "Processing…" messages want.
export function createStatusLine(id, { ms: defaultMs, color: defaultColor } = {}) {
  const node = el(id);
  let timer = null;
  return function show(message, color = defaultColor, ms = defaultMs) {
    if (!node) return;
    node.textContent = message;
    node.style.color = color;
    if (ms) {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => { node.textContent = ''; }, ms);
    }
  };
}
