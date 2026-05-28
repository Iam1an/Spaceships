import * as THREE from 'three';

const MISSILE_SPEED = 160;   // units/second
const TURN_RATE     = 1.4;   // radians/second angular limit
const LIFE          = 8.0;   // seconds before self-destruct
const HIT_RADIUS    = 6.0;   // ship hit-detection sphere radius

// ── Avoidance tuning ─────────────────────────────────────────────────────────
// Lookahead is dynamic: max(BASE, obstacle_radius × RADIUS_SCALE).
// At 280 u/s and TURN_RATE 1.8 rad/s the missile can deflect ~44° in the
// base window — enough to clear most rocks. The moon gets ~3× more notice
// because its radius is huge (80 u), giving the missile ~89° of turning room.
const AVOID_BASE_LOOKAHEAD = 130;   // units ahead to scan along forward
const AVOID_RADIUS_SCALE   = 3.2;   // lookahead multiplier per obstacle radius
const AVOID_MARGIN         = 4.0;   // clearance added on top of obstacle radius
const MISSILE_AVOID_R      = 2.5;   // missile's own "radius" for avoidance
const AVOID_WEIGHT         = 4.0;   // how strongly avoidance blends into homing
// Hard detonation: missile explodes if it physically enters an obstacle.
// Slightly larger than the missile body so fast approaches don't tunnel through.
const DETONATE_MARGIN      = 2.0;

// ── Missile silhouette dimensions (everything along local +Z) ──────────────
const BODY_LEN  = 3.5;
const BODY_RAD  = 0.28;
const NOSE_LEN  = 1.8;
const FIN_SPAN  = 2.0;
const FIN_THICK = 0.07;
const FIN_DEPTH = 1.1;
const FIN_Z     = -(BODY_LEN / 2 - FIN_DEPTH / 2 - 0.1);   // ≈ –1.1
const NOZZLE_Z  = -(BODY_LEN / 2 + 0.18);                   // ≈ –1.93

// ── Shared geometry (built once at module load) ───────────────────────────────
const noseGeo = (() => {
  const g = new THREE.ConeGeometry(BODY_RAD, NOSE_LEN, 10, 1, false);
  g.rotateX(-Math.PI / 2);
  g.translate(0, 0, BODY_LEN / 2 + NOSE_LEN / 2);
  return g;
})();

const fuselageGeo = (() => {
  const g = new THREE.CylinderGeometry(BODY_RAD, BODY_RAD + 0.04, BODY_LEN, 10, 1);
  g.rotateX(Math.PI / 2);
  return g;
})();

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

const nozzleBellGeo = (() => {
  const g = new THREE.ConeGeometry(0.38, 0.55, 10, 1, false);
  g.rotateX(Math.PI / 2);
  g.rotateX(Math.PI);   // open end faces backward (–Z)
  g.translate(0, 0, NOZZLE_Z - 0.1);
  return g;
})();

const nozzleGlowGeo = (() => {
  const g = new THREE.SphereGeometry(0.55, 10, 7);
  g.translate(0, 0, NOZZLE_Z);
  return g;
})();

const trailGeo     = new THREE.SphereGeometry(0.5, 6, 4);
const explosionGeo = new THREE.SphereGeometry(1.0, 10, 7);

const TRAIL_INTERVAL = 0.028;   // seconds between trail particle spawns

export function createMissiles() {
  const group = new THREE.Group();
  group.name = 'Missiles';

  // Static shared body materials — never disposed per-missile.
  const bodyMat = new THREE.MeshBasicMaterial({ color: 0xd4dce8 });
  const finMat  = new THREE.MeshBasicMaterial({ color: 0x7a8fa8 });
  const bellMat = new THREE.MeshBasicMaterial({ color: 0x445566 });

  const missiles       = [];
  const trailParticles = [];
  const explosions     = [];

  // ── Scratch vectors (reused every frame, safe because JS is single-threaded) ──
  const _fwd      = new THREE.Vector3(0, 0, 1);
  const _toTgt    = new THREE.Vector3();
  const _currDir  = new THREE.Vector3();
  const _desired  = new THREE.Vector3();
  const _newDir   = new THREE.Vector3();
  const _avoidOut = new THREE.Vector3();
  const _tailWS   = new THREE.Vector3();

  // ── Obstacle avoidance ────────────────────────────────────────────────────
  // Cast along `dir` from `origin`. For every asteroid / static obstacle whose
  // closest approach along that ray is within AVOID_BASE_LOOKAHEAD (or larger
  // for big obstacles), accumulate a perpendicular push away from it weighted
  // by urgency (1 at foot-of-ray, 0 at lookahead limit). Returns a normalised
  // push vector if anything is in the way, or null if the path is clear.
  function computeAvoidance(origin, dir, asteroids, obstacles) {
    _avoidOut.set(0, 0, 0);
    let any = false;

    function consider(cx, cy, cz, radius) {
      const px = cx - origin.x, py = cy - origin.y, pz = cz - origin.z;
      // Signed projection of obstacle centre onto forward ray.
      const t = px * dir.x + py * dir.y + pz * dir.z;
      const lookAhead = Math.max(AVOID_BASE_LOOKAHEAD, radius * AVOID_RADIUS_SCALE);
      // Skip if clearly behind or beyond lookahead window.
      if (t < -radius || t > lookAhead) return;
      // Closest point on the ray to the obstacle centre.
      const ax = origin.x + dir.x * t;
      const ay = origin.y + dir.y * t;
      const az = origin.z + dir.z * t;
      const dx = ax - cx, dy = ay - cy, dz = az - cz;
      const distSq = dx * dx + dy * dy + dz * dz;
      const threshold = radius + MISSILE_AVOID_R + AVOID_MARGIN;
      if (distSq > threshold * threshold) return; // ray misses the obstacle
      const urgency = 1.0 - Math.max(0, t) / lookAhead;
      const dist = Math.sqrt(distSq);
      if (dist < 1e-3) {
        // Perfectly head-on — push sideways (deterministic right-hand turn).
        _avoidOut.x += -dir.z * urgency;
        _avoidOut.z +=  dir.x * urgency;
      } else {
        _avoidOut.x += (dx / dist) * urgency;
        _avoidOut.y += (dy / dist) * urgency;
        _avoidOut.z += (dz / dist) * urgency;
      }
      any = true;
    }

    if (asteroids) {
      for (const a of asteroids.list) {
        consider(
          a.mesh.position.x, a.mesh.position.y, a.mesh.position.z,
          a.radius,
        );
      }
    }
    if (obstacles) {
      for (const o of obstacles) {
        consider(o.pos.x, o.pos.y, o.pos.z, o.radius);
      }
    }
    return any ? _avoidOut.normalize() : null;
  }

  // Hard detonation check: is the missile position inside any obstacle?
  function insideObstacle(pos, asteroids, obstacles) {
    if (asteroids) {
      for (const a of asteroids.list) {
        const dx = pos.x - a.mesh.position.x;
        const dy = pos.y - a.mesh.position.y;
        const dz = pos.z - a.mesh.position.z;
        const r = a.radius + DETONATE_MARGIN;
        if (dx * dx + dy * dy + dz * dz < r * r) return true;
      }
    }
    if (obstacles) {
      for (const o of obstacles) {
        const dx = pos.x - o.pos.x;
        const dy = pos.y - o.pos.y;
        const dz = pos.z - o.pos.z;
        const r = o.radius + DETONATE_MARGIN;
        if (dx * dx + dy * dy + dz * dz < r * r) return true;
      }
    }
    return false;
  }

  // ── Visual helpers ────────────────────────────────────────────────────────
  function makeMissileMesh() {
    const root = new THREE.Group();
    root.add(new THREE.Mesh(noseGeo,       bodyMat));
    root.add(new THREE.Mesh(fuselageGeo,   bodyMat));
    root.add(new THREE.Mesh(finHGeo,       finMat));
    root.add(new THREE.Mesh(finVGeo,       finMat));
    root.add(new THREE.Mesh(nozzleBellGeo, bellMat));
    const glowMat = new THREE.MeshBasicMaterial({
      color: 0xff9900, transparent: true, opacity: 0.88,
      blending: THREE.AdditiveBlending, depthWrite: false,
    });
    const nozzle = new THREE.Mesh(nozzleGlowGeo, glowMat);
    root.add(nozzle);
    return { root, nozzle, glowMat };
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
    const layers = [
      { color: 0xffffff, from: 0.8,  to:  5.0, life: 0.30 },
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

  // ── Public API ────────────────────────────────────────────────────────────
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

  // asteroids  – the asteroids object from main.js ({ list: [...] })
  // obstacles  – static obstacle array from main.js ([{ pos, radius }])
  function update(dt, remoteShips, onHitRemote, shooterTeam, asteroids, obstacles) {

    // ── Missile physics ───────────────────────────────────────────────────────
    for (let i = missiles.length - 1; i >= 0; i--) {
      const m = missiles[i];
      m.life -= dt;
      m.age  += dt;
      let consumed = m.life <= 0;

      if (!consumed) {
        // ① Current forward direction in world space.
        _currDir.set(0, 0, 1).applyQuaternion(m.mesh.quaternion);

        // ② Homing: desired direction toward live target.
        let hasTarget = false;
        const tgt = m.target;
        if (tgt && tgt.alive && tgt.ship) {
          _toTgt.copy(tgt.ship.position).sub(m.mesh.position);
          const d = _toTgt.length();
          if (d > 0.5) {
            _toTgt.divideScalar(d);   // normalise in-place
            hasTarget = true;
          }
        }
        // Fall back to flying straight if target is gone.
        _desired.copy(hasTarget ? _toTgt : _currDir);

        // ③ Obstacle avoidance: blend a perpendicular push into the desired dir.
        const avoid = computeAvoidance(m.mesh.position, _currDir, asteroids, obstacles);
        if (avoid !== null) {
          _desired.addScaledVector(avoid, AVOID_WEIGHT).normalize();
        }

        // ④ Apply turn-rate limit: rotate toward _desired by at most TURN_RATE*dt.
        const dot = Math.max(-1, Math.min(1, _currDir.dot(_desired)));
        const angleDiff = Math.acos(dot);
        if (angleDiff > 0.001) {
          const factor = Math.min(1, (TURN_RATE * dt) / angleDiff);
          _newDir.lerpVectors(_currDir, _desired, factor).normalize();
          m.mesh.quaternion.setFromUnitVectors(_fwd, _newDir);
          m.vel.copy(_newDir).multiplyScalar(MISSILE_SPEED);
        }

        // ⑤ Integrate position.
        m.mesh.position.addScaledVector(m.vel, dt);

        // ⑥ Hard detonation: missile has entered an obstacle despite avoidance.
        if (insideObstacle(m.mesh.position, asteroids, obstacles)) {
          spawnExplosion(m.mesh.position.clone());
          consumed = true;
        }
      } else {
        // Lifetime expired — small puff.
        spawnExplosion(m.mesh.position.clone());
      }

      if (!consumed) {
        // Nozzle glow flicker.
        const pulse = 0.75 + 0.45 * Math.abs(Math.sin(m.age * 19.0));
        m.nozzle.scale.setScalar(pulse);
        m.glowMat.opacity = 0.70 + 0.25 * pulse;

        // Trail: emit at nozzle world-space position.
        m.trailTimer += dt;
        while (m.trailTimer >= TRAIL_INTERVAL) {
          m.trailTimer -= TRAIL_INTERVAL;
          _tailWS.set(0, 0, NOZZLE_Z)
            .applyQuaternion(m.mesh.quaternion)
            .add(m.mesh.position);
          emitTrail(_tailWS);
        }

        // Ship hit detection.
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
      }

      if (consumed) {
        group.remove(m.mesh);
        m.glowMat.dispose();
        missiles.splice(i, 1);
      }
    }

    // ── Trail particle animation ──────────────────────────────────────────────
    for (let i = trailParticles.length - 1; i >= 0; i--) {
      const p = trailParticles[i];
      p.age += dt;
      const t = p.age / p.life;
      p.mesh.scale.setScalar(p.initScale * (1.0 + t * 2.8));
      p.mesh.material.opacity = (1 - t) * 0.72;
      if (t >= 1) {
        group.remove(p.mesh);
        p.mesh.material.dispose();
        trailParticles.splice(i, 1);
      }
    }

    // ── Explosion animation ───────────────────────────────────────────────────
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
