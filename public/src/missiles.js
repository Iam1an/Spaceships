import * as THREE from 'three';
const MISSILE_SPEED = 160;
const TURN_RATE = 1.4;
const LIFE = 8.0;
const HIT_RADIUS = 6.0;
const AVOID_BASE_LOOKAHEAD = 130;
const AVOID_RADIUS_SCALE = 3.2;
const AVOID_MARGIN = 4.0;
const MISSILE_AVOID_R = 2.5;
const AVOID_WEIGHT = 4.0;
const DETONATE_MARGIN = 2.0;
const FLARE_SPEED = 140;
const FLARE_LIFE = 1.8;
const FLARE_COUNT = 20;
const FLARE_SEDUCTION_DIST = 180;
const FLARE_TRAIL_INTERVAL = 0.030;
const BODY_LEN = 3.5;
const BODY_RAD = 0.28;
const NOSE_LEN = 1.8;
const FIN_SPAN = 2.0;
const FIN_THICK = 0.07;
const FIN_DEPTH = 1.1;
const FIN_Z = -(BODY_LEN / 2 - FIN_DEPTH / 2 - 0.1);
const NOZZLE_Z = -(BODY_LEN / 2 + 0.18);
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
  g.rotateX(Math.PI);
  g.translate(0, 0, NOZZLE_Z - 0.1);
  return g;
})();
const nozzleGlowGeo = (() => {
  const g = new THREE.SphereGeometry(0.55, 10, 7);
  g.translate(0, 0, NOZZLE_Z);
  return g;
})();
const trailGeo = new THREE.SphereGeometry(0.5, 6, 4);
const explosionGeo = new THREE.SphereGeometry(1.0, 10, 7);
const flareCoreSphereGeo = new THREE.SphereGeometry(0.30, 8, 6);
const flareGlowSphereGeo = new THREE.SphereGeometry(1.10, 10, 8);
const TRAIL_INTERVAL = 0.028;
export function createMissiles() {
  const group = new THREE.Group();
  group.name = 'Missiles';
  const bodyMat = new THREE.MeshBasicMaterial({ color: 0xd4dce8 });
  const finMat = new THREE.MeshBasicMaterial({ color: 0x7a8fa8 });
  const bellMat = new THREE.MeshBasicMaterial({ color: 0x445566 });
  const missiles = [];
  const trailParticles = [];
  const explosions = [];
  const flares = [];
  const _fwd = new THREE.Vector3(0, 0, 1);
  const _toTgt = new THREE.Vector3();
  const _currDir = new THREE.Vector3();
  const _desired = new THREE.Vector3();
  const _newDir = new THREE.Vector3();
  const _avoidOut = new THREE.Vector3();
  const _tailWS = new THREE.Vector3();
  function computeAvoidance(origin, dir, asteroids, obstacles) {
    _avoidOut.set(0, 0, 0);
    let any = false;
    function consider(cx, cy, cz, radius) {
      const px = cx - origin.x, py = cy - origin.y, pz = cz - origin.z;
      const t = px * dir.x + py * dir.y + pz * dir.z;
      const lookAhead = Math.max(AVOID_BASE_LOOKAHEAD, radius * AVOID_RADIUS_SCALE);
      if (t < -radius || t > lookAhead) return;
      const ax = origin.x + dir.x * t;
      const ay = origin.y + dir.y * t;
      const az = origin.z + dir.z * t;
      const dx = ax - cx, dy = ay - cy, dz = az - cz;
      const distSq = dx * dx + dy * dy + dz * dz;
      const threshold = radius + MISSILE_AVOID_R + AVOID_MARGIN;
      if (distSq > threshold * threshold) return;
      const urgency = 1.0 - Math.max(0, t) / lookAhead;
      const dist = Math.sqrt(distSq);
      if (dist < 1e-3) {
        _avoidOut.x += -dir.z * urgency;
        _avoidOut.z += dir.x * urgency;
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
  function makeMissileMesh() {
    const root = new THREE.Group();
    root.add(new THREE.Mesh(noseGeo, bodyMat));
    root.add(new THREE.Mesh(fuselageGeo, bodyMat));
    root.add(new THREE.Mesh(finHGeo, finMat));
    root.add(new THREE.Mesh(finVGeo, finMat));
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
    trailParticles.push({ mesh, age: 0, life: 0.30 + Math.random() * 0.12, initScale: s, maxOpacity: 0.72 });
  }
  function emitFlareTrail(pos) {
    const isBright = Math.random() > 0.38;
    const col = isBright
      ? (Math.random() > 0.5 ? 0xffffff : 0xffee66)
      : (Math.random() > 0.5 ? 0xff9922 : 0xff5500);
    const maxOp = isBright ? 0.92 : 0.68;
    const mat = new THREE.MeshBasicMaterial({
      color: col, transparent: true, opacity: maxOp,
      blending: THREE.AdditiveBlending, depthWrite: false,
    });
    const mesh = new THREE.Mesh(trailGeo, mat);
    mesh.position.set(
      pos.x + (Math.random() - 0.5) * 0.5,
      pos.y + (Math.random() - 0.5) * 0.5,
      pos.z + (Math.random() - 0.5) * 0.5,
    );
    const s = isBright
      ? 1.0 + Math.random() * 1.2
      : 2.0 + Math.random() * 1.8;
    mesh.scale.setScalar(s);
    group.add(mesh);
    trailParticles.push({
      mesh, age: 0,
      life: isBright
        ? 0.22 + Math.random() * 0.14
        : 0.38 + Math.random() * 0.20,
      initScale: s,
      maxOpacity: maxOp,
    });
  }
  function spawnExplosion(pos) {
    const layers = [
      { color: 0xffffff, from: 0.8, to: 5.0, life: 0.30 },
      { color: 0xff9900, from: 1.4, to: 11.0, life: 0.52 },
      { color: 0xff3300, from: 2.0, to: 16.0, life: 0.70 },
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
  function spawnFlareBurst(pos) {
    const burstData = [
      { color: 0xffffff, from: 0.15, to: 3.5, life: 0.18 },
      { color: 0xffee44, from: 0.40, to: 6.5, life: 0.26 },
      { color: 0xff8800, from: 0.70, to: 10.0, life: 0.32 },
    ];
    for (const l of burstData) {
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
  function deployFlare(origin, shipQuaternion, ownerId) {
    spawnFlareBurst(origin);
    for (let i = 0; i < FLARE_COUNT; i++) {
      const theta = Math.random() * Math.PI * 2;
      const cosP = Math.random() * 2 - 1;
      const sinP = Math.sqrt(1 - cosP * cosP);
      const dir = new THREE.Vector3(
        sinP * Math.cos(theta),
        sinP * Math.sin(theta),
        cosP,
      ).applyQuaternion(shipQuaternion);
      const speed = FLARE_SPEED * (0.65 + Math.random() * 0.70);
      const coreMat = new THREE.MeshBasicMaterial({
        color: 0xffffff, transparent: true, opacity: 1.0,
        blending: THREE.AdditiveBlending, depthWrite: false,
      });
      const coreMesh = new THREE.Mesh(flareCoreSphereGeo, coreMat);
      const glowMat = new THREE.MeshBasicMaterial({
        color: 0xffcc22, transparent: true, opacity: 0.70,
        blending: THREE.AdditiveBlending, depthWrite: false,
      });
      const glowMesh = new THREE.Mesh(flareGlowSphereGeo, glowMat);
      const flareGroup = new THREE.Group();
      flareGroup.position.copy(origin);
      flareGroup.add(coreMesh);
      flareGroup.add(glowMesh);
      group.add(flareGroup);
      const flare = {
        group: flareGroup,
        coreMesh, coreMat,
        glowMesh, glowMat,
        vel: dir.clone().multiplyScalar(speed),
        life: FLARE_LIFE,
        age: 0,
        trailTimer: 0,
        alive: true,
        ownerId: ownerId ?? null,
      };
      flare.targetRecord = {
        isFlare: true,
        get alive() { return flare.alive; },
        ship: flareGroup,
      };
      flares.push(flare);
    }
  }
  function isTargetingLocal(localRecord) {
    if (!localRecord) return false;
    for (const m of missiles) {
      if (m.target === localRecord) return true;
    }
    return false;
  }
  function fire(origin, direction, targetRecord, ownerId, ownerTeam) {
    const normDir = direction.clone().normalize();
    const { root, nozzle, glowMat } = makeMissileMesh();
    root.position.copy(origin);
    root.quaternion.setFromUnitVectors(_fwd, normDir);
    group.add(root);
    missiles.push({
      mesh: root,
      nozzle,
      glowMat,
      vel: normDir.multiplyScalar(MISSILE_SPEED),
      target: targetRecord,
      life: LIFE,
      age: 0,
      trailTimer: 0,
      ownerId: ownerId ?? null,
      ownerTeam: ownerTeam ?? null,
    });
  }
  function update(dt, remoteShips, onHitRemote, shooterTeam, asteroids, obstacles, localTarget) {
    for (let i = missiles.length - 1; i >= 0; i--) {
      const m = missiles[i];
      m.life -= dt;
      m.age += dt;
      let consumed = m.life <= 0;
      if (!consumed) {
        _currDir.set(0, 0, 1).applyQuaternion(m.mesh.quaternion);
        if (m.target && !m.target.isFlare && flares.length > 0) {
          let nearestFlare = null;
          let nearestFlareDist = FLARE_SEDUCTION_DIST;
          for (const f of flares) {
            if (!f.alive) continue;
            if (m.ownerId && f.ownerId && m.ownerId === f.ownerId) continue;
            const fd = m.mesh.position.distanceTo(f.group.position);
            if (fd < nearestFlareDist) { nearestFlareDist = fd; nearestFlare = f; }
          }
          if (nearestFlare) m.target = nearestFlare.targetRecord;
        }
        let hasTarget = false;
        const tgt = m.target;
        if (tgt) {
          if (!tgt.alive) {
            m.target = null;
          } else if (tgt.ship) {
            _toTgt.copy(tgt.ship.position).sub(m.mesh.position);
            const d = _toTgt.length();
            if (d > 0.5) {
              _toTgt.divideScalar(d);
              hasTarget = true;
            }
          }
        }
        _desired.copy(hasTarget ? _toTgt : _currDir);
        const avoid = computeAvoidance(m.mesh.position, _currDir, asteroids, obstacles);
        if (avoid !== null) {
          _desired.addScaledVector(avoid, AVOID_WEIGHT).normalize();
        }
        const dot = Math.max(-1, Math.min(1, _currDir.dot(_desired)));
        const angleDiff = Math.acos(dot);
        if (angleDiff > 0.001) {
          const factor = Math.min(1, (TURN_RATE * dt) / angleDiff);
          _newDir.lerpVectors(_currDir, _desired, factor).normalize();
          m.mesh.quaternion.setFromUnitVectors(_fwd, _newDir);
          m.vel.copy(_newDir).multiplyScalar(MISSILE_SPEED);
        }
        m.mesh.position.addScaledVector(m.vel, dt);
        if (insideObstacle(m.mesh.position, asteroids, obstacles)) {
          spawnExplosion(m.mesh.position.clone());
          consumed = true;
        }
      } else {
        spawnExplosion(m.mesh.position.clone());
      }
      if (!consumed) {
        const pulse = 0.75 + 0.45 * Math.abs(Math.sin(m.age * 19.0));
        m.nozzle.scale.setScalar(pulse);
        m.glowMat.opacity = 0.70 + 0.25 * pulse;
        m.trailTimer += dt;
        while (m.trailTimer >= TRAIL_INTERVAL) {
          m.trailTimer -= TRAIL_INTERVAL;
          _tailWS.set(0, 0, NOZZLE_Z)
            .applyQuaternion(m.mesh.quaternion)
            .add(m.mesh.position);
          emitTrail(_tailWS);
        }
        if (flares.length > 0) {
          for (let fi = flares.length - 1; fi >= 0; fi--) {
            const f = flares[fi];
            if (!f.alive) continue;
            const fdx = m.mesh.position.x - f.group.position.x;
            const fdy = m.mesh.position.y - f.group.position.y;
            const fdz = m.mesh.position.z - f.group.position.z;
            if (fdx * fdx + fdy * fdy + fdz * fdz < HIT_RADIUS * HIT_RADIUS) {
              spawnExplosion(m.mesh.position.clone());
              f.alive = false;
              group.remove(f.group);
              f.coreMat.dispose();
              f.glowMat.dispose();
              flares.splice(fi, 1);
              consumed = true;
              break;
            }
          }
        }
        const mTeam = m.ownerTeam !== null ? m.ownerTeam : shooterTeam;
        if (remoteShips && !consumed) {
          for (const [id, r] of remoteShips) {
            if (!r.alive) continue;
            if (id === m.ownerId) continue;
            if (mTeam !== undefined && mTeam !== null && r.team === mTeam) continue;
            const dx = m.mesh.position.x - r.ship.position.x;
            const dy = m.mesh.position.y - r.ship.position.y;
            const dz = m.mesh.position.z - r.ship.position.z;
            if (dx * dx + dy * dy + dz * dz < HIT_RADIUS * HIT_RADIUS) {
              spawnExplosion(m.mesh.position.clone());
              if (onHitRemote) onHitRemote(id, m.ownerId, m.ownerTeam);
              consumed = true;
              break;
            }
          }
        }
        if (localTarget && !consumed
          && m.ownerId !== localTarget.id
          && localTarget.record.alive
          && !(mTeam !== undefined && mTeam !== null && mTeam === localTarget.team)) {
          const dx = m.mesh.position.x - localTarget.record.ship.position.x;
          const dy = m.mesh.position.y - localTarget.record.ship.position.y;
          const dz = m.mesh.position.z - localTarget.record.ship.position.z;
          if (dx * dx + dy * dy + dz * dz < HIT_RADIUS * HIT_RADIUS) {
            spawnExplosion(m.mesh.position.clone());
            if (localTarget.onHit) localTarget.onHit(m.ownerId, m.ownerTeam);
            consumed = true;
          }
        }
      }
      if (consumed) {
        group.remove(m.mesh);
        m.glowMat.dispose();
        missiles.splice(i, 1);
      }
    }
    for (let i = trailParticles.length - 1; i >= 0; i--) {
      const p = trailParticles[i];
      p.age += dt;
      const t = p.age / p.life;
      p.mesh.scale.setScalar(p.initScale * (1.0 + t * 2.8));
      p.mesh.material.opacity = (1 - t) * (p.maxOpacity ?? 0.72);
      if (t >= 1) {
        group.remove(p.mesh);
        p.mesh.material.dispose();
        trailParticles.splice(i, 1);
      }
    }
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
    for (let i = flares.length - 1; i >= 0; i--) {
      const f = flares[i];
      f.age += dt;
      f.life -= dt;
      if (f.life <= 0) {
        f.alive = false;
        group.remove(f.group);
        f.coreMat.dispose();
        f.glowMat.dispose();
        flares.splice(i, 1);
        continue;
      }
      f.group.position.addScaledVector(f.vel, dt);
      f.vel.multiplyScalar(Math.pow(0.22, dt));
      const tLife = 1.0 - f.life / FLARE_LIFE;
      const cFlick = 0.70 + 0.30 * Math.abs(Math.sin(f.age * 34.0 + i * 1.7));
      const gFlick = 0.55 + 0.45 * Math.abs(Math.sin(f.age * 19.0 + i * 3.1));
      const coreScale = THREE.MathUtils.lerp(1.4, 0.20, tLife) * cFlick;
      f.coreMesh.scale.setScalar(Math.max(0.05, coreScale));
      f.coreMat.opacity = Math.min(1.0, (1.0 - tLife * 0.45) * cFlick);
      const glowScale = THREE.MathUtils.lerp(2.2, 0.8, tLife) * (0.9 + 0.1 * gFlick);
      f.glowMesh.scale.setScalar(Math.max(0.1, glowScale));
      f.glowMat.opacity = (1.0 - tLife * 0.75) * gFlick * 0.70;
      f.trailTimer += dt;
      while (f.trailTimer >= FLARE_TRAIL_INTERVAL) {
        f.trailTimer -= FLARE_TRAIL_INTERVAL;
        emitFlareTrail(f.group.position.clone());
      }
    }
  }
  return { group, fire, update, deployFlare, isTargetingLocal };
}