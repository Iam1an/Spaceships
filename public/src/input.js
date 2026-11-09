export class Input {
  constructor(domElement) {
    this.keys = new Set();
    this.lmb = false;
    this.rmb = false;
    this.mouseDisabled = false;
    this.touchEnabled = false;
    this.steerX = 0;
    this.steerY = 0;
    this.virtualX = 0;
    this.virtualY = 0;
    this.wheel = 0;
    this.throttleOverride = null;
    this.dx = 0;
    this.dy = 0;
    this.stickActive = false;
    this.stickBaseX = 0;
    this.stickBaseY = 0;
    this.stickKnobX = 0;
    this.stickKnobY = 0;
    this._stickId = null;
    this._stickRadius = 80;
    this.gp = {
      steerX: 0,
      steerY: 0,
      rollAxis: 0,
      throttleAxis: 0,
      fire: false,
      drift: false,
      boost: false,
      menuBtn: false,
      freeLook: false,
    };
    this._gpPrevMissile = false;
    this._gpPrevFlare = false;
    this._gpPrevGunToggle = false;
    this._gpPrevMenuBtn = false;
    window.addEventListener('keydown', (e) => {
      this.keys.add(e.code);
      if (e.code === 'Space'
        || e.code === 'ArrowUp' || e.code === 'ArrowDown'
        || e.code === 'ArrowLeft' || e.code === 'ArrowRight') {
        e.preventDefault();
      }
    });
    window.addEventListener('keyup', (e) => this.keys.delete(e.code));
    window.addEventListener('blur', () => this.keys.clear());
    domElement.addEventListener('contextmenu', (e) => e.preventDefault());
    domElement.addEventListener('mousedown', (e) => {
      if (this.mouseDisabled) return;
      if (e.button === 0) this.lmb = true;
      if (e.button === 2) this.rmb = true;
    });
    window.addEventListener('mouseup', (e) => {
      if (this.mouseDisabled) return;
      if (e.button === 0) this.lmb = false;
      if (e.button === 2) this.rmb = false;
    });
    window.addEventListener('mousemove', (e) => {
      if (this.mouseDisabled) return;
      if (this.rmb) {
        this.dx += e.movementX;
        this.dy += e.movementY;
        return;
      }
      const halfH = window.innerHeight * 0.5;
      const halfW = window.innerWidth * 0.5;
      if (document.pointerLockElement) {
        this.virtualX = Math.max(-halfH, Math.min(halfH, this.virtualX + e.movementX));
        this.virtualY = Math.max(-halfH, Math.min(halfH, this.virtualY + e.movementY));
        this.steerX = Math.max(-1, Math.min(1, this.virtualX / halfH));
        this.steerY = Math.max(-1, Math.min(1, this.virtualY / halfH));
      } else {
        const nx = (e.clientX - halfW) / halfH;
        const ny = (e.clientY - halfH) / halfH;
        this.steerX = Math.max(-1, Math.min(1, nx));
        this.steerY = Math.max(-1, Math.min(1, ny));
        this.virtualX = nx * halfH;
        this.virtualY = ny * halfH;
      }
    });
    window.addEventListener('wheel', (e) => {
      if (this.mouseDisabled) return;
      this.wheel += -Math.sign(e.deltaY);
      e.preventDefault();
    }, { passive: false });
    const isControlTouch = (t) => !!t.target?.closest?.('[data-touch-control]');
    const onTouchStart = (e) => {
      if (!this.touchEnabled) return;
      let claimed = false;
      for (const t of e.changedTouches) {
        if (isControlTouch(t)) continue;
        if (this._stickId !== null) continue;
        if (t.clientX >= window.innerWidth * 0.5) continue;
        this._stickId = t.identifier;
        this.stickActive = true;
        this.stickBaseX = t.clientX;
        this.stickBaseY = t.clientY;
        this.stickKnobX = t.clientX;
        this.stickKnobY = t.clientY;
        this.steerX = 0;
        this.steerY = 0;
        claimed = true;
      }
      if (claimed) e.preventDefault();
    };
    const onTouchMove = (e) => {
      if (!this.touchEnabled) return;
      let claimed = false;
      for (const t of e.changedTouches) {
        if (t.identifier !== this._stickId) continue;
        const dx = t.clientX - this.stickBaseX;
        const dy = t.clientY - this.stickBaseY;
        const r = this._stickRadius;
        const len = Math.hypot(dx, dy);
        const k = len > r ? r / len : 1;
        const kx = dx * k, ky = dy * k;
        this.stickKnobX = this.stickBaseX + kx;
        this.stickKnobY = this.stickBaseY + ky;
        this.steerX = kx / r;
        this.steerY = ky / r;
        claimed = true;
      }
      if (claimed) e.preventDefault();
    };
    const onTouchEnd = (e) => {
      if (!this.touchEnabled) return;
      for (const t of e.changedTouches) {
        if (t.identifier === this._stickId) {
          this._stickId = null;
          this.stickActive = false;
          this.steerX = 0;
          this.steerY = 0;
        }
      }
    };
    window.addEventListener('touchstart', onTouchStart, { passive: false });
    window.addEventListener('touchmove', onTouchMove, { passive: false });
    window.addEventListener('touchend', onTouchEnd);
    window.addEventListener('touchcancel', onTouchEnd);
  }
  consumeMouseDelta() {
    const dx = this.dx, dy = this.dy;
    this.dx = 0;
    this.dy = 0;
    return { dx, dy };
  }
  consumeWheel() {
    const w = this.wheel;
    this.wheel = 0;
    return w;
  }
  pollGamepad() {
    const DEAD = 0.12;
    const dead = (v) => {
      const a = Math.abs(v);
      return a < DEAD ? 0 : Math.sign(v) * (a - DEAD) / (1 - DEAD);
    };
    const gamepad = [...(navigator.getGamepads?.() ?? [])].find(g => g?.connected);
    if (!gamepad) {
      this.gp.steerX = 0;
      this.gp.steerY = 0;
      this.gp.rollAxis = 0;
      this.gp.throttleAxis = 0;
      this.gp.fire = false;
      this.gp.drift = false;
      this.gp.boost = false;
      this.gp.freeLook = false;
      return;
    }
    const ax = gamepad.axes;
    const bt = gamepad.buttons;
    const btn = (i) => bt[i]?.pressed ?? false;
    const val = (i) => bt[i]?.value ?? (btn(i) ? 1 : 0);
    this.gp.steerX = dead(ax[2] ?? 0);
    this.gp.steerY = dead(ax[3] ?? 0);
    this.gp.rollAxis = dead(ax[0] ?? 0);
    this.gp.throttleAxis = -dead(ax[1] ?? 0);
    this.gp.fire = val(7) > 0.5 || btn(0);
    this.gp.drift = val(6) > 0.5;
    this.gp.boost = btn(4);
    // R3 (right-stick click) held = free-look with the right stick. The right stick already
    // steers the ship on gamepad, so head-look needs this modifier rather than the bare stick.
    this.gp.freeLook = btn(11);
    const gpMissile = btn(5) || btn(2);
    const gpFlare = btn(1);
    const gpGunToggle = btn(3);
    if (gpMissile && !this._gpPrevMissile) this.keys.add('KeyE');
    if (!gpMissile && this._gpPrevMissile) this.keys.delete('KeyE');
    if (gpFlare && !this._gpPrevFlare) this.keys.add('KeyQ');
    if (!gpFlare && this._gpPrevFlare) this.keys.delete('KeyQ');
    if (gpGunToggle && !this._gpPrevGunToggle) this.keys.add('KeyP');
    if (!gpGunToggle && this._gpPrevGunToggle) this.keys.delete('KeyP');
    this._gpPrevMissile = gpMissile;
    this._gpPrevFlare = gpFlare;
    this._gpPrevGunToggle = gpGunToggle;
    const gpMenuBtn = btn(9);
    this.gp.menuBtn = gpMenuBtn && !this._gpPrevMenuBtn;
    this._gpPrevMenuBtn = gpMenuBtn;
  }
}
