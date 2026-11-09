const BUTTON_ATTR = 'data-touch-control';
export function createTouchHud({ input, scheme }) {
  if (scheme !== 'mobile') return { update() { }, destroy() { } };
  const root = document.createElement('div');
  root.id = 'touchhud';
  root.style.cssText = [
    'position:fixed', 'inset:0',
    'pointer-events:none',
    'z-index:3',
    'user-select:none', '-webkit-user-select:none',
    '-webkit-touch-callout:none',
    'font-family:inherit',
  ].join(';');
  document.body.appendChild(root);
  const stickEl = document.createElement('div');
  stickEl.style.cssText = 'position:absolute;left:0;top:0;pointer-events:none;opacity:0;transition:opacity 0.12s ease;';
  const stickBaseEl = document.createElement('div');
  stickBaseEl.style.cssText = [
    'position:absolute', 'width:160px', 'height:160px',
    'margin:-80px 0 0 -80px',
    'border:3px solid rgba(176,224,255,0.55)',
    'border-radius:50%',
    'background:rgba(8,14,28,0.25)',
    'box-shadow:0 0 18px rgba(80,160,255,0.35)',
  ].join(';');
  const stickKnobEl = document.createElement('div');
  stickKnobEl.style.cssText = [
    'position:absolute', 'width:64px', 'height:64px',
    'margin:-32px 0 0 -32px',
    'border:3px solid #ffd97a',
    'border-radius:50%',
    'background:rgba(255,217,122,0.35)',
    'box-shadow:0 0 14px #ffd97a',
  ].join(';');
  stickEl.appendChild(stickBaseEl);
  stickEl.appendChild(stickKnobEl);
  root.appendChild(stickEl);
  const allButtons = [];
  function makeButton(label, opts) {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = label;
    b.setAttribute(BUTTON_ATTR, '');
    b.style.cssText = [
      'position:absolute',
      'pointer-events:auto',
      'font-family:inherit',
      `font-size:${opts.fontSize || 11}px`,
      'letter-spacing:1px',
      'color:#fff',
      'border-radius:50%',
      `background:${opts.bg || 'rgba(8,14,28,0.7)'}`,
      `border:3px solid ${opts.border || '#b0e0ff'}`,
      `box-shadow:0 0 14px ${opts.glow || 'rgba(80,160,255,0.4)'}`,
      `width:${opts.size || 80}px`, `height:${opts.size || 80}px`,
      'text-shadow:1px 1px 0 #000',
      'cursor:pointer',
      'transition:transform 0.06s ease, opacity 0.1s ease',
      'touch-action:none',
      'opacity:0.85',
    ].join(';');
    Object.assign(b.style, opts.position || {});
    const press = (e) => {
      e.preventDefault();
      b.style.transform = 'scale(0.92)';
      b.style.opacity = '1';
      try { b.setPointerCapture?.(e.pointerId); } catch { }
      opts.onHold(true);
    };
    const release = (e) => {
      b.style.transform = '';
      b.style.opacity = '0.85';
      try { b.releasePointerCapture?.(e.pointerId); } catch { }
      opts.onHold(false);
    };
    b.addEventListener('pointerdown', press);
    b.addEventListener('pointerup', release);
    b.addEventListener('pointercancel', release);
    b.addEventListener('pointerleave', (e) => {
      if (b.hasPointerCapture?.(e.pointerId)) release(e);
    });
    root.appendChild(b);
    allButtons.push({ el: b, release: () => opts.onHold(false) });
    return b;
  }
  function holdKey(code) {
    return (down) => { down ? input.keys.add(code) : input.keys.delete(code); };
  }
  function holdLmb() {
    return (down) => { input.lmb = down; };
  }
  makeButton('FIRE', {
    size: 120,
    bg: 'linear-gradient(180deg, #ff7070 0%, #c22a2a 100%)',
    border: '#ffb0b0', glow: 'rgba(255,80,80,0.6)',
    fontSize: 14,
    position: { right: '24px', bottom: '24px' },
    onHold: holdLmb(),
  });
  makeButton('DRIFT', {
    size: 72,
    bg: 'linear-gradient(180deg, #ffe07a 0%, #ff8833 100%)',
    border: '#ffd97a', glow: 'rgba(255,200,80,0.55)',
    position: { right: '168px', bottom: '24px' },
    onHold: holdKey('Space'),
  });
  makeButton('BOOST', {
    size: 72,
    bg: 'linear-gradient(180deg, #66ddff 0%, #2867d0 100%)',
    border: '#b0e0ff', glow: 'rgba(80,160,255,0.55)',
    position: { right: '168px', bottom: '108px' },
    onHold: holdKey('ShiftLeft'),
  });
  makeButton('⟲', {
    size: 64, fontSize: 28,
    position: { right: '240px', bottom: '192px' },
    onHold: holdKey('KeyA'),
  });
  makeButton('⟳', {
    size: 64, fontSize: 28,
    position: { right: '168px', bottom: '192px' },
    onHold: holdKey('KeyD'),
  });
  makeButton('MSL', {
    size: 64,
    bg: 'linear-gradient(180deg, #ffaa66 0%, #cc4411 100%)',
    border: '#ffcc99', glow: 'rgba(255,140,60,0.55)',
    position: { right: '264px', bottom: '24px' },
    onHold: holdKey('KeyE'),
  });
  makeButton('FLARE', {
    size: 64, fontSize: 10,
    bg: 'linear-gradient(180deg, #fff0a0 0%, #d09020 100%)',
    border: '#ffe680', glow: 'rgba(255,220,100,0.55)',
    position: { right: '264px', bottom: '108px' },
    onHold: holdKey('KeyQ'),
  });
  const slider = document.createElement('div');
  slider.setAttribute(BUTTON_ATTR, '');
  slider.style.cssText = [
    'position:absolute', 'right:60px', 'bottom:160px',
    'width:40px', 'height:220px',
    'pointer-events:auto', 'touch-action:none',
    'background:rgba(8,14,28,0.7)',
    'border:2px solid #b0e0ff',
    'border-radius:22px',
    'box-shadow:0 0 10px rgba(80,160,255,0.35)',
  ].join(';');
  const sliderFill = document.createElement('div');
  sliderFill.style.cssText = [
    'position:absolute', 'left:0', 'right:0', 'bottom:0',
    'height:0%',
    'background:linear-gradient(180deg, #66ddff 0%, #2867d0 100%)',
    'border-radius:20px', 'pointer-events:none',
  ].join(';');
  const sliderThumb = document.createElement('div');
  sliderThumb.style.cssText = [
    'position:absolute', 'left:50%', 'bottom:0',
    'width:48px', 'height:24px',
    'margin:0 0 -12px -24px',
    'background:#ffd97a',
    'border:2px solid #fff',
    'border-radius:12px',
    'box-shadow:0 0 12px #ffd97a',
    'pointer-events:none',
  ].join(';');
  const sliderLabel = document.createElement('div');
  sliderLabel.textContent = 'THR';
  sliderLabel.style.cssText = [
    'position:absolute', 'left:0', 'right:0', 'top:-20px',
    'text-align:center', 'font-size:11px',
    'color:#b0c8e0', 'letter-spacing:2px',
    'pointer-events:none',
  ].join(';');
  slider.appendChild(sliderFill);
  slider.appendChild(sliderThumb);
  slider.appendChild(sliderLabel);
  root.appendChild(slider);
  function setThrottleFromTouchY(clientY) {
    const rect = slider.getBoundingClientRect();
    const y = clientY - rect.top;
    const f = Math.max(0, Math.min(1, 1 - y / rect.height));
    input.throttleOverride = f;
    const pct = (f * 100).toFixed(0);
    sliderFill.style.height = pct + '%';
    sliderThumb.style.bottom = pct + '%';
  }
  let sliderPointer = null;
  slider.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    sliderPointer = e.pointerId;
    try { slider.setPointerCapture(e.pointerId); } catch { }
    setThrottleFromTouchY(e.clientY);
  });
  slider.addEventListener('pointermove', (e) => {
    if (e.pointerId !== sliderPointer) return;
    setThrottleFromTouchY(e.clientY);
  });
  const releaseSlider = (e) => {
    if (e.pointerId !== sliderPointer) return;
    sliderPointer = null;
    try { slider.releasePointerCapture(e.pointerId); } catch { }
  };
  slider.addEventListener('pointerup', releaseSlider);
  slider.addEventListener('pointercancel', releaseSlider);
  const onBlur = () => { for (const { release } of allButtons) release(); };
  window.addEventListener('blur', onBlur);
  function update() {
    if (!stickEl) return;
    if (input.stickActive) {
      stickEl.style.opacity = '1';
      stickBaseEl.style.left = input.stickBaseX + 'px';
      stickBaseEl.style.top = input.stickBaseY + 'px';
      stickKnobEl.style.left = input.stickKnobX + 'px';
      stickKnobEl.style.top = input.stickKnobY + 'px';
    } else {
      stickEl.style.opacity = '0';
    }
  }
  function destroy() {
    window.removeEventListener('blur', onBlur);
    root.remove();
  }
  return { update, destroy };
}