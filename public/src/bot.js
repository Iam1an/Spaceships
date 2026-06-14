import * as THREE from 'three';
export function createBotAI(record, deps) {
  const {
    getOpponents, team, beams, bullets, asteroids, obstacles = [],
    raySphereDist, solveIntercept, audio, distanceVol,
    faction = 'enemy',
    onFire,
    hardMode = false,
    terrainHeightFn = null,
    fireMissile = null,
    missileMax = 0,
  } = deps;
  const ZERO_VEC = new THREE.Vector3();
  const SPEED = 60;
  const TURN_RATE = 1.3;
  const ACCEL = 3;
  const FIRE_RANGE = 600;
  const FIRE_DOT = 0.97;
  const FIRE_COOLDOWN = hardMode ? 0.05 : 0.15;
  const DAMAGE = 10;
  const MISSILE_MIN_RANGE = 130;
  const MISSILE_MAX_RANGE = 560;
  const MISSILE_FIRE_DOT = 0.90;
  const MISSILE_COOLDOWN = 8.0;
  const missileDelay = () => 2.5 + Math.random() * 4.0;
  const BULLET_SPEED = 780;
  const BULLET_LIFE = 2.0;
  const SEEK_DIST = 250;
  const ATTACK_TOO_CLOSE = 35;
  const EVADE_DURATION = 0.6;
  const SHIP_RADIUS = 3.5;
  const AIM_OFFSET_MAX = 14;
  const AIM_OFFSET_DRIFT = 12;
  const AIM_REF_DIST = 200;
  const AIM_TRACK_RATE = 10;
  const AVOID_LOOKAHEAD = 80;
  const AVOID_MARGIN = 4;
  const AVOID_WEIGHT = 2.0;
  const STUCK_SPEED_THRESH = 6;
  const STUCK_TIME = 1.5;
  let state = 'seek';
  let stateTimer = 0;
  let fireTimer = 0;
  let missilesLeft = missileMax;
  let missileTimer = missileDelay();
  let stuckTime = 0;
  let evadeAxis = new THREE.Vector3();
  const aimOffset = new THREE.Vector3();
  const trackedLead = new THREE.Vector3();
  let trackedLeadSeeded = false;
  const myProjectiles = [];
  const BULLET_HIT_R = SHIP_RADIUS + 0.5;
  const tmpFwd = new THREE.Vector3();
  const tmpAxis = new THREE.Vector3();
  const tmpQuat = new THREE.Quaternion();
  const tmpToTarget = new THREE.Vector3();
  function chooseEvadeDir() {
    return new THREE.Vector3(
      Math.random() * 2 - 1,
      Math.random() * 2 - 1,
      Math.random() * 2 - 1,
    ).normalize();
  }
  function rotateToward(quaternion, fromDir, toDir, maxAngle) {
    const angle = fromDir.angleTo(toDir);
    if (angle < 1e-3) return;
    const step = Math.min(maxAngle, angle);
    tmpAxis.crossVectors(fromDir, toDir);
    if (tmpAxis.lengthSq() < 1e-6) {
      tmpAxis.set(0, 1, 0);
      if (Math.abs(fromDir.y) > 0.9) tmpAxis.set(1, 0, 0);
    } else {
      tmpAxis.normalize();
    }
    tmpQuat.setFromAxisAngle(tmpAxis, step);
    quaternion.premultiply(tmpQuat);
    quaternion.normalize();
  }
  const tmpAvoid = new THREE.Vector3();
  function computeAvoidance(origin, dir) {
    tmpAvoid.set(0, 0, 0);
    let any = false;
    function consider(cx, cy, cz, radius) {
      const px = cx - origin.x;
      const py = cy - origin.y;
      const pz = cz - origin.z;
      const t = px * dir.x + py * dir.y + pz * dir.z;
      const lookAhead = Math.max(AVOID_LOOKAHEAD, radius * 2.5);
      if (t < 0 || t > lookAhead) return;
      const ax = origin.x + dir.x * t;
      const ay = origin.y + dir.y * t;
      const az = origin.z + dir.z * t;
      const dx = ax - cx;
      const dy = ay - cy;
      const dz = az - cz;
      const distSq = dx * dx + dy * dy + dz * dz;
      const threshold = radius + SHIP_RADIUS + AVOID_MARGIN;
      if (distSq > threshold * threshold) return;
      const urgency = 1 - t / lookAhead;
      const dist = Math.sqrt(distSq);
      if (dist < 1e-3) {
        tmpAvoid.x += -dir.z * urgency;
        tmpAvoid.z += dir.x * urgency;
      } else {
        tmpAvoid.x += (dx / dist) * urgency;
        tmpAvoid.y += (dy / dist) * urgency;
        tmpAvoid.z += (dz / dist) * urgency;
      }
      any = true;
    }
    for (const a of asteroids.list) {
      consider(a.mesh.position.x, a.mesh.position.y, a.mesh.position.z, a.radius);
    }
    for (const o of obstacles) {
      consider(o.pos.x, o.pos.y, o.pos.z, o.radius);
    }
    return any ? tmpAvoid.normalize() : null;
  }
  function pickTarget() {
    let best = null;
    let bestDist = Infinity;
    const here = record.ship.position;
    for (const e of getOpponents()) {
      if (!e.alive) continue;
      const d = e.position.distanceTo(here);
      if (d < bestDist) { bestDist = d; best = e; }
    }
    return best;
  }
  function update(dt) {
    updateProjectiles(dt);
    if (!record.alive) return;
    stateTimer += dt;
    fireTimer -= dt;
    missileTimer -= dt;
    const target = pickTarget();
    if (!target) {
      tmpFwd.set(0, 0, 1).applyQuaternion(record.ship.quaternion);
      const targetVel = tmpFwd.clone().multiplyScalar(SPEED * 0.3);
      record.vel.lerp(targetVel, 1 - Math.exp(-ACCEL * dt));
      record.ship.position.addScaledVector(record.vel, dt);
      record.targetPos.copy(record.ship.position);
      record.targetQuat.copy(record.ship.quaternion);
      record.hasTarget = true;
      return;
    }
    const targetPos = target.position;
    const targetVel = target.velocity;
    const botPos = record.ship.position;
    const dist = targetPos.distanceTo(botPos);
    if (state === 'seek' && dist < SEEK_DIST) {
      state = 'attack';
      stateTimer = 0;
    } else if (state === 'attack') {
      if (dist < ATTACK_TOO_CLOSE) {
        state = 'evade';
        stateTimer = 0;
        evadeAxis = chooseEvadeDir();
      } else if (dist > SEEK_DIST * 1.3) {
        state = 'seek';
        stateTimer = 0;
      }
    } else if (state === 'evade' && stateTimer >= EVADE_DURATION) {
      state = 'seek';
      stateTimer = 0;
    }
    aimOffset.x += (Math.random() - 0.5) * AIM_OFFSET_DRIFT * dt;
    aimOffset.y += (Math.random() - 0.5) * AIM_OFFSET_DRIFT * dt;
    aimOffset.z += (Math.random() - 0.5) * AIM_OFFSET_DRIFT * dt;
    const aimMaxSq = AIM_OFFSET_MAX * AIM_OFFSET_MAX;
    if (aimOffset.lengthSq() > aimMaxSq) aimOffset.setLength(AIM_OFFSET_MAX);
    const leadT = solveIntercept(targetPos, targetVel, botPos, ZERO_VEC, BULLET_SPEED);
    const leadPoint = leadT !== null && Number.isFinite(leadT)
      ? targetPos.clone().addScaledVector(targetVel, leadT)
      : targetPos.clone();
    if (!trackedLeadSeeded) {
      trackedLead.copy(leadPoint);
      trackedLeadSeeded = true;
    } else {
      trackedLead.lerp(leadPoint, 1 - Math.exp(-AIM_TRACK_RATE * dt));
    }
    const errorScale = dist / AIM_REF_DIST;
    const aimWorld = trackedLead.clone().addScaledVector(aimOffset, errorScale);
    let desiredDir;
    if (state === 'evade') {
      desiredDir = evadeAxis.clone();
    } else {
      desiredDir = aimWorld.clone().sub(botPos).normalize();
    }
    tmpFwd.set(0, 0, 1).applyQuaternion(record.ship.quaternion);
    const avoid = computeAvoidance(botPos, tmpFwd);
    if (avoid) {
      desiredDir.addScaledVector(avoid, AVOID_WEIGHT).normalize();
    }
    if (terrainHeightFn !== null) {
      const margin = 180;
      const groundBelow = terrainHeightFn(botPos.x, botPos.z);
      const clearanceBelow = botPos.y - groundBelow;
      const ahead = botPos.clone().addScaledVector(tmpFwd, SPEED * 1.5);
      const groundAhead = terrainHeightFn(ahead.x, ahead.z);
      const clearanceAhead = botPos.y - groundAhead;
      const clearance = Math.min(clearanceBelow, clearanceAhead);
      if (clearance < margin) {
        const pull = (margin - clearance) / margin;
        desiredDir.y += pull * 6.0;
        if (desiredDir.length() > 0.001) desiredDir.normalize();
      }
    }
    rotateToward(record.ship.quaternion, tmpFwd, desiredDir, TURN_RATE * dt);
    tmpFwd.set(0, 0, 1).applyQuaternion(record.ship.quaternion);
    const tv = tmpFwd.clone().multiplyScalar(SPEED);
    record.vel.lerp(tv, 1 - Math.exp(-ACCEL * dt));
    botPos.addScaledVector(record.vel, dt);
    for (const o of obstacles) {
      const dx = botPos.x - o.pos.x;
      const dy = botPos.y - o.pos.y;
      const dz = botPos.z - o.pos.z;
      const distSq = dx * dx + dy * dy + dz * dz;
      const minDist = SHIP_RADIUS + o.radius;
      if (distSq < minDist * minDist && distSq > 0.0001) {
        const dist = Math.sqrt(distSq);
        const nx = dx / dist, ny = dy / dist, nz = dz / dist;
        botPos.x += nx * (minDist - dist);
        botPos.y += ny * (minDist - dist);
        botPos.z += nz * (minDist - dist);
        const vDotN = record.vel.x * nx + record.vel.y * ny + record.vel.z * nz;
        if (vDotN < 0) {
          record.vel.x -= 1.3 * vDotN * nx;
          record.vel.y -= 1.3 * vDotN * ny;
          record.vel.z -= 1.3 * vDotN * nz;
        }
        state = 'evade';
        stateTimer = 0;
        evadeAxis = chooseEvadeDir();
      }
    }
    for (const a of asteroids.list) {
      const dx = botPos.x - a.mesh.position.x;
      const dy = botPos.y - a.mesh.position.y;
      const dz = botPos.z - a.mesh.position.z;
      const distSq = dx * dx + dy * dy + dz * dz;
      const minDist = SHIP_RADIUS + a.radius;
      if (distSq < minDist * minDist && distSq > 0.0001) {
        const dist = Math.sqrt(distSq);
        const nx = dx / dist, ny = dy / dist, nz = dz / dist;
        botPos.x += nx * (minDist - dist);
        botPos.y += ny * (minDist - dist);
        botPos.z += nz * (minDist - dist);
        const vDotN = record.vel.x * nx + record.vel.y * ny + record.vel.z * nz;
        if (vDotN < 0) {
          record.vel.x -= 1.3 * vDotN * nx;
          record.vel.y -= 1.3 * vDotN * ny;
          record.vel.z -= 1.3 * vDotN * nz;
        }
        if (state !== 'evade') {
          state = 'evade';
          stateTimer = 0;
          evadeAxis = chooseEvadeDir();
        }
      }
    }
    if (terrainHeightFn !== null) {
      const groundY = terrainHeightFn(botPos.x, botPos.z);
      if (botPos.y < groundY + 5) {
        botPos.y = groundY + 5;
        if (record.vel.y < 0) record.vel.y *= -0.5;
      }
    }
    record.targetPos.copy(botPos);
    record.targetQuat.copy(record.ship.quaternion);
    record.hasTarget = true;
    if (record.vel.lengthSq() < STUCK_SPEED_THRESH * STUCK_SPEED_THRESH && state !== 'evade') {
      stuckTime += dt;
      if (stuckTime >= STUCK_TIME) {
        state = 'evade';
        stateTimer = 0;
        evadeAxis = chooseEvadeDir();
        stuckTime = 0;
      }
    } else {
      stuckTime = 0;
    }
    if (state === 'attack' && fireTimer <= 0 && dist < FIRE_RANGE) {
      const ideal = aimWorld.clone().sub(botPos).normalize();
      if (tmpFwd.dot(ideal) > FIRE_DOT) {
        fireBullet();
        fireTimer = FIRE_COOLDOWN;
      }
    }
    if (missilesLeft > 0 && fireMissile && state === 'attack'
      && missileTimer <= 0 && dist > MISSILE_MIN_RANGE && dist < MISSILE_MAX_RANGE) {
      tmpToTarget.copy(targetPos).sub(botPos).normalize();
      if (tmpFwd.dot(tmpToTarget) > MISSILE_FIRE_DOT) {
        if (fireMissile(target) !== false) {
          missilesLeft--;
          missileTimer = MISSILE_COOLDOWN;
        }
      }
    }
  }
  function fireBullet() {
    const botPos = record.ship.position;
    tmpFwd.set(0, 0, 1).applyQuaternion(record.ship.quaternion);
    const start = botPos.clone().addScaledVector(tmpFwd, 2.5);
    if (bullets) bullets.fire(start, tmpFwd, faction);
    if (onFire) onFire(start, tmpFwd);
    myProjectiles.push({
      pos: start.clone(),
      vel: tmpFwd.clone().multiplyScalar(BULLET_SPEED),
      life: BULLET_LIFE,
    });
    if (audio && distanceVol) audio.play('shoot', distanceVol(botPos));
  }
  function updateProjectiles(dt) {
    for (let i = myProjectiles.length - 1; i >= 0; i--) {
      const p = myProjectiles[i];
      p.life -= dt;
      if (p.life <= 0) { myProjectiles.splice(i, 1); continue; }
      p.pos.addScaledVector(p.vel, dt);
      let consumed = false;
      for (const e of getOpponents()) {
        if (!e.alive) continue;
        const dx = p.pos.x - e.position.x;
        const dy = p.pos.y - e.position.y;
        const dz = p.pos.z - e.position.z;
        if (dx * dx + dy * dy + dz * dz < BULLET_HIT_R * BULLET_HIT_R) {
          e.takeHit(DAMAGE, record.id, team);
          if (bullets) bullets.spawnExplosion(p.pos.clone(), 1.0);
          consumed = true;
          break;
        }
      }
      if (consumed) { myProjectiles.splice(i, 1); continue; }
      for (const a of asteroids.list) {
        const dx = p.pos.x - a.mesh.position.x;
        const dy = p.pos.y - a.mesh.position.y;
        const dz = p.pos.z - a.mesh.position.z;
        const r = a.radius + 0.5;
        if (dx * dx + dy * dy + dz * dz < r * r) {
          myProjectiles.splice(i, 1);
          consumed = true;
          break;
        }
      }
      if (consumed) continue;
      for (const o of obstacles) {
        const dx = p.pos.x - o.pos.x;
        const dy = p.pos.y - o.pos.y;
        const dz = p.pos.z - o.pos.z;
        const r = o.radius + 0.5;
        if (dx * dx + dy * dy + dz * dz < r * r) {
          myProjectiles.splice(i, 1);
          break;
        }
      }
    }
  }
  function notifyHit() {
    if (!record.alive) return;
    if (state !== 'evade') {
      state = 'evade';
      stateTimer = 0;
      evadeAxis = chooseEvadeDir();
    }
  }
  function notifyRespawn() {
    state = 'seek';
    stateTimer = 0;
    fireTimer = 0;
    missilesLeft = missileMax;
    missileTimer = missileDelay();
    record.vel.set(0, 0, 0);
  }
  return { update, notifyHit, notifyRespawn };
}