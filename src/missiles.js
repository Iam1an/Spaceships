import * as THREE from 'three';

const MISSILE_SPEED = 280;   // units/second
const TURN_RATE     = 1.8;   // radians/second angular limit
const LIFE          = 8.0;   // seconds before self-destruct
const HIT_RADIUS    = 6.0;   // hit-detection sphere radius

// ── Missile silhouette dimensions (everything along local +Z) ──────────────
const BODY_LEN  = 3.5;
const BODY_RAD  = 0.28;
const NOSE_LEN  = 1.8;
const FIN_SPAN  = 2.0;   // total wingspan (both sides)
const FIN_THICK = 0.07;
const FIN_DEPTH = 1.1;
// Fins sit in the rear third of the body
const FIN_Z = -(BODY_LEN / 2 - FIN_DEPTH / 2 - 0.1);   // ≈ –1.1
// Nozzle glow sits just behind the tail
const NOZZLE_Z  = -(BODY_LEN / 2 + 0.18);               // ≈ –1.93

// ── Shared geometry – built once at module load, reused by every missile ────
// ConeGeometry default axis is +Y; rotating –90° around X makes the tip
// point along +Z; then we translate it to sit in front of the fuselage.
const noseGeo = (() => {
  const g = new THREE.ConeGeometry(BODY_RAD, NOSE_LEN, 10, 1, false);
  g.rotateX(-Math.PI / 2);
  g.translate(0, 0, BODY_LEN / 2 + NOSE_LEN / 2);
  return g;
})();

// CylinderGeometry axis is +Y; rotating 90° around X gives axis = +Z.
const fuselageGeo = (() => {
  const g = new THREE.CylinderGeometry(BODY_RAD, BODY_RAD + 0.04, BODY_LEN, 10, 1);
  g.rotateX(Math.PI / 2);
  return g;
})();

// Two perpendicular flat boxes form a + cross-fin at the tail.
const finHGeo = (() => {
  const g = new THREE.BoxGeometry(FIN_SPAN, FIN_THICK, FIN_DEPTH);
  g.translate(0, 0, FIN_Z);
  return g;
})();

const finVGeo = (() => {
  const g = new THREE.BoxGeometry(FIN_THICK, FIN_SPAN, FIN_DEPTH);
  g.translate(0, 0, FIN_Z);
  return g;
})();

// Small cone for the tail nozzle bell (tip points backward = toward –Z).
const nozzleBellGeo = (() => {
  const g = new THREE.ConeGeometry(0.38, 0.55, 10, 1, false);
  g.rotateX(Math.PI / 2);   // tip now points along +Z (forward); flip below
  g.rotateX(Math.PI);       // flip so opening faces backward
  g.translate(0, 0, NOZZLE_Z - 0.1);
  return g;
})();

// Sphere used for both trail particles and explosion layers (different sizes).
const trailGeo     = new THREE.SphereGeometry(0.5, 6, 4);
const explosionGeo = new THREE.SphereGeometry(1.0, 10, 7);

// Nozzle glow – shared geometry, per-instance material so each missile can
// animate opacity/scale independently.
const nozzleGlowGeo = (() => {
  const g = new THREE.SphereGeometry(0.55, 10, 7);
  g.translate(0, 0, NOZZLE_Z);
  return g;
})();

// ── Trail-emission cadence ───────────────────────────────────────────────────
const TRAIL_INTERVAL = 0.028;   // seconds between particle spawns ≈ 35/sec

export function createMissiles() {
  const group = new THREE.Group();
  group.name = 'Missiles';

  // Shared, static materials for the missile body – never disposed per-missile.
  const bodyMat = new THREE.MeshBasicMaterial({ color: 0xd4dce8 });
  const finMat  = new THREE.MeshBasicMaterial({ color: 0x7a8fa8 });
  const bellMat = new THREE.MeshBasicMaterial({ color: 0x445566 });

  const missiles      = [];
  const trailParticles = [];
  const explosions    = [];

  // Pre-allocated scratch vectors – never used across yield points so safe to share.
  const _fwd     = new THREE.Vector3(0, 0, 1);
  const _toTgt   = new THREE.Vector3();
  const _currDir = new THREE.Vector3();
  const _newDir  = new THREE.Vector3();
  const _tailWS  = new THREE.Vector3();

  // Build the visual group for one missile; returns root + ref to nozzle mesh.
  function makeMissileMesh() {
    const root = new THREE.Group();
    root.add(new THREE.Mesh(noseGeo,      bodyMat));
    root.add(new THREE.Mesh(fuselageGeo,  bodyMat));
    root.add(new THREE.Mesh(finHGeo,      finMat));
    root.add(new THREE.Mesh(finVGeo,      finMat));
    root.add(new THREE.Mesh(nozzleBellGeo, bellMat));

    // Per-instance nozzle glow so opacity/scale can vary per missile.
    const glowMat = new THREE.MeshBasicMaterial({
      color: 0xff9900,
      transparent: true, opacity: 0.88,
      blending: THREE.AdditiveBlending, depthWrite: false,
    });
    const nozzle = new THREE.Mesh(nozzleGlowGeo, glowMat);
    root.add(nozzle);
    return { root, nozzle, glowMat };
  }

  function fire(origin, direction, targetRecord) {
    const normDir = direction.clone().normalize();
    const { root, nozzle, glowMat } = makeMissileMesh();
    root.position.copy(origin);
    root.quaternion.setFromUnitVectors(_fwd, normDir);
    group.add(root);
    missiles.push({
      mesh: root,
      nozzle,
      glowMat,
      vel:        normDir.multiplyScalar(MISSILE_SPEED),
      target:     targetRecord,
      life:       LIFE,
      age:        0,
      trailTimer: 0,
    });
  }

  function emitTrail(pos) {
    const mat = new THREE.MeshBasicMaterial({
      color: 0xff7700, transparent: true, opacity: 0.72,
      blending: THREE.AdditiveBlending, depthWrite: false,
    });
    const mesh = new THREE.Mesh(trailGeo, mat);
    mesh.position.copy(pos);
    const s = 0.45 + Math.random() * 0.65;
    mesh.scale.setScalar(s);
    group.add(mesh);
    trailParticles.push({ mesh, age: 0, life: 0.30 + Math.random() * 0.12, initScale: s });
  }

  function spawnExplosion(pos) {
    // Three concentric bursts: white core, orange mid, red outer.
    const layers = [
      { color: 0xffffff, from: 0.8,  to: 5.0,  life: 0.30 },
      { color: 0xff9900, from: 1.4,  to: 11.0, life: 0.52 },
      { color: 0xff3300, from: 2.0,  to: 16.0, life: 0.70 },
    ];
    for (const l of layers) {
      const mat = new THREE.MeshBasicMaterial({
        color: l.color, transparent: true, opacity: 0.95,
        blending: THREE.AdditiveBlending, depthWrite: false,
      });
      const mesh = new THREE.Mesh(explosionGeo, mat);
      mesh.position.copy(pos);
      mesh.scale.setScalar(l.from);
      group.add(mesh);
      explosions.push({ mesh, age: 0, life: l.life, from: l.from, to: l.to });
    }
  }

  function update(dt, remoteShips, onHitRemote, shooterTeam) {
    // ── Missile physics + homing ─────────────────────────────────────────────
    for (let i = missiles.length - 1; i >= 0; i--) {
      const m = missiles[i];
      m.life -= dt;
      m.age  += dt;
      let consumed = m.life <= 0;

      if (!consumed) {
        // Proportional steering toward live target.
        const tgt = m.target;
        if (tgt && tgt.alive && tgt.ship) {
          _toTgt.copy(tgt.ship.position).sub(m.mesh.position);
          const dist = _toTgt.length();
          if (dist > 0.5) {
            _toTgt.divideScalar(dist);
            _currDir.set(0, 0, 1).applyQuaternion(m.mesh.quaternion);
            const dot = Math.max(-1, Math.min(1, _currDir.dot(_toTgt)));
            const angleDiff = Math.acos(dot);
            if (angleDiff > 0.001) {
              const factor = Math.min(1, (TURN_RATE * dt) / angleDiff);
              _newDir.lerpVectors(_currDir, _toTgt, factor).normalize();
              m.mesh.quaternion.setFromUnitVectors(_fwd, _newDir);
              m.vel.copy(_newDir).multiplyScalar(MISSILE_SPEED);
            }
          }
        }

        m.mesh.position.addScaledVector(m.vel, dt);

        // Nozzle glow pulse – simulates engine combustion flicker.
        const pulse = 0.75 + 0.45 * Math.abs(Math.sin(m.age * 19.0));
        m.nozzle.scale.setScalar(pulse);
        m.glowMat.opacity = 0.7 + 0.25 * pulse;

        // Trail: emit a particle at the nozzle position (world space).
        m.trailTimer += dt;
        while (m.trailTimer >= TRAIL_INTERVAL) {
          m.trailTimer -= TRAIL_INTERVAL;
          _tailWS.set(0, 0, NOZZLE_Z)
            .applyQuaternion(m.mesh.quaternion)
            .add(m.mesh.position);
          emitTrail(_tailWS);
        }

        // Hit detection against remote ships (and local bots in solo).
        if (remoteShips) {
          for (const [id, r] of remoteShips) {
            if (!r.alive) continue;
            if (shooterTeam !== undefined && shooterTeam !== null && r.team === shooterTeam) continue;
            const dx = m.mesh.position.x - r.ship.position.x;
            const dy = m.mesh.position.y - r.ship.position.y;
            const dz = m.mesh.position.z - r.ship.position.z;
            if (dx * dx + dy * dy + dz * dz < HIT_RADIUS * HIT_RADIUS) {
              spawnExplosion(m.mesh.position.clone());
              if (onHitRemote) onHitRemote(id);
              consumed = true;
              break;
            }
          }
        }
      } else {
        spawnExplosion(m.mesh.position.clone());
      }

      if (consumed) {
        group.remove(m.mesh);
        m.glowMat.dispose();   // per-instance material
        missiles.splice(i, 1);
      }
    }

    // ── Trail particle animation ─────────────────────────────────────────────
    for (let i = trailParticles.length - 1; i >= 0; i--) {
      const p = trailParticles[i];
      p.age += dt;
      const t = p.age / p.life;
      // Expand as they drift and fade.
      p.mesh.scale.setScalar(p.initScale * (1.0 + t * 2.8));
      p.mesh.material.opacity = (1 - t) * 0.72;
      if (t >= 1) {
        group.remove(p.mesh);
        p.mesh.material.dispose();
        trailParticles.splice(i, 1);
      }
    }

    // ── Explosion animation ──────────────────────────────────────────────────
    for (let i = explosions.length - 1; i >= 0; i--) {
      const e = explosions[i];
      e.age += dt;
      const t = e.age / e.life;
      e.mesh.scale.setScalar(THREE.MathUtils.lerp(e.from, e.to, t));
      e.mesh.material.opacity = (1 - t) * 0.95;
      if (t >= 1) {
        group.remove(e.mesh);
        e.mesh.material.dispose();
        explosions.splice(i, 1);
      }
    }
  }

  return { group, fire, update };
}
