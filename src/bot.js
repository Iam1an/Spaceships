import * as THREE from 'three';

// Bot AI driving a player-shaped `record`. Steering + reactive state machine:
// SEEK chases at distance, ATTACK lines up an intercept and fires beams,
// EVADE breaks off after damage or when too close. Aims with the same
// intercept solver the player's targeting computer uses, so it leads.
//
// Generic over targets: `deps.getOpponents()` returns entity-like objects
// with { position, velocity, alive, takeHit(dmg, killerTeam) }. Bot picks
// the closest live opponent, aims at it, and fires beams that can hit any
// opponent in the line of sight (or asteroids that block the shot).
export function createBotAI(record, deps) {
  const {
    getOpponents, team, beams, bullets, asteroids, obstacles = [],
    raySphereDist, solveIntercept, audio, distanceVol,
    faction = 'enemy',
    onFire,
    hardMode = false,
  } = deps;
  const ZERO_VEC = new THREE.Vector3();

  const SPEED = 60;
  const TURN_RATE = 1.3;
  const ACCEL = 3;
  const FIRE_RANGE = 600;
  // Projectile aim — bot fires bullets that travel at finite speed, leads
  // its targets via the same intercept solver the player uses.
  const FIRE_DOT = 0.97;        // ~14° cone
  // Hard mode matches the player's 20 shots/sec (0.05s cooldown). Default
  // is the original 6.7/sec so bots stay beatable for casual play.
  const FIRE_COOLDOWN = hardMode ? 0.05 : 0.15;
  const DAMAGE = 10;            // matches player bullet damage (buffed)
  const BULLET_SPEED = 780;
  const BULLET_LIFE = 2.0;
  const SEEK_DIST = 250;
  const ATTACK_TOO_CLOSE = 35;
  const EVADE_DURATION = 0.6;   // was 1.2 — bots come back for the kill faster
  const SHIP_RADIUS = 3.5;
  // Aim wobble: bot's intended aim is offset from target by a slowly-drifting
  // vector. Scaled by distance so the angular error is the same at any range
  // (~2-3° max at the current values), giving a "shaky human pilot" feel.
  const AIM_OFFSET_MAX = 14;
  const AIM_OFFSET_DRIFT = 12;
  const AIM_REF_DIST = 200;
  // Tracking lag: the bot aims at a damped follower of the true lead
  // point, not the lead point directly. Makes the aim slide onto target
  // over ~0.10s instead of snapping, which keeps a tiny human-feel
  // hesitation without being a free dodge window.
  const AIM_TRACK_RATE = 10;

  // Steering avoidance: lookahead along current forward, perpendicular push
  // away from any asteroid the ray would clip. Blends into desiredDir.
  const AVOID_LOOKAHEAD = 80;
  const AVOID_MARGIN = 4;
  const AVOID_WEIGHT = 2.0;
  // Stuck detector: if velocity stays low for too long, kick into a random
  // evade axis to break wedged-against-an-asteroid situations.
  const STUCK_SPEED_THRESH = 6;
  const STUCK_TIME = 1.5;

  let state = 'seek';
  let stateTimer = 0;
  let fireTimer = 0;
  let stuckTime = 0;
  let evadeAxis = new THREE.Vector3();
  const aimOffset = new THREE.Vector3();
  // Where the bot currently *thinks* the lead point is. Lerps toward
  // the freshly-computed lead each frame so aim slides, not snaps.
  const trackedLead = new THREE.Vector3();
  let trackedLeadSeeded = false;
  // Per-bot projectile ledger. bullets.js renders the visual; this list is
  // for hit detection, since bullets.js only resolves hits for the local
  // player's own bullets.
  const myProjectiles = [];
  const BULLET_HIT_R = SHIP_RADIUS + 0.5;

  const tmpFwd = new THREE.Vector3();
  const tmpAxis = new THREE.Vector3();
  const tmpQuat = new THREE.Quaternion();

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

  // Steering-style obstacle avoidance: cast forward along `dir` from `origin`,
  // sum perpendicular pushes away from any asteroid that would clip the
  // path. Returns a unit vector to blend with the desired direction, or
  // null if nothing's in the way.
  const tmpAvoid = new THREE.Vector3();
  function computeAvoidance(origin, dir) {
    tmpAvoid.set(0, 0, 0);
    let any = false;
    // Single sphere-avoidance pass over asteroids + static obstacles
    // (moon). Same shape handling for both — only the source list and
    // radius differ.
    function consider(cx, cy, cz, radius) {
      const px = cx - origin.x;
      const py = cy - origin.y;
      const pz = cz - origin.z;
      const t = px * dir.x + py * dir.y + pz * dir.z;
      // Scale lookahead by obstacle size so large obstacles (moon, radius=80)
      // are seen ~2.5× further out than small asteroids.
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
        // Head-on: pick a deterministic perpendicular (right-hand turn).
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
    // Bullets keep ticking even while the bot is dead — gives in-flight
    // shots a chance to land instead of vanishing on death.
    updateProjectiles(dt);
    if (!record.alive) return;

    stateTimer += dt;
    fireTimer -= dt;

    const target = pickTarget();
    if (!target) {
      // No opponents — drift on current heading.
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

    // State transitions
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

    // Drift the aim wobble each frame and clamp to a sphere.
    aimOffset.x += (Math.random() - 0.5) * AIM_OFFSET_DRIFT * dt;
    aimOffset.y += (Math.random() - 0.5) * AIM_OFFSET_DRIFT * dt;
    aimOffset.z += (Math.random() - 0.5) * AIM_OFFSET_DRIFT * dt;
    const aimMaxSq = AIM_OFFSET_MAX * AIM_OFFSET_MAX;
    if (aimOffset.lengthSq() > aimMaxSq) aimOffset.setLength(AIM_OFFSET_MAX);

    // Compute the bullet-lead intercept point — bullets travel at finite
    // speed in world frame (no shipVel inheritance), so selfVel = 0.
    const leadT = solveIntercept(targetPos, targetVel, botPos, ZERO_VEC, BULLET_SPEED);
    const leadPoint = leadT !== null && Number.isFinite(leadT)
      ? targetPos.clone().addScaledVector(targetVel, leadT)
      : targetPos.clone();
    // Track the lead point with a lag so a darting target slips out of
    // the bot's reticle for a moment before it catches up.
    if (!trackedLeadSeeded) {
      trackedLead.copy(leadPoint);
      trackedLeadSeeded = true;
    } else {
      trackedLead.lerp(leadPoint, 1 - Math.exp(-AIM_TRACK_RATE * dt));
    }
    const errorScale = dist / AIM_REF_DIST;
    const aimWorld = trackedLead.clone().addScaledVector(aimOffset, errorScale);

    // Desired direction = lead point + wobble.
    let desiredDir;
    if (state === 'evade') {
      desiredDir = evadeAxis.clone();
    } else {
      desiredDir = aimWorld.clone().sub(botPos).normalize();
    }

    // Blend in obstacle avoidance: lookahead along current heading, push
    // perpendicular off any asteroid in the way.
    tmpFwd.set(0, 0, 1).applyQuaternion(record.ship.quaternion);
    const avoid = computeAvoidance(botPos, tmpFwd);
    if (avoid) {
      desiredDir.addScaledVector(avoid, AVOID_WEIGHT).normalize();
    }

    // Rotate toward desired
    rotateToward(record.ship.quaternion, tmpFwd, desiredDir, TURN_RATE * dt);

    // Move along forward
    tmpFwd.set(0, 0, 1).applyQuaternion(record.ship.quaternion);
    const tv = tmpFwd.clone().multiplyScalar(SPEED);
    record.vel.lerp(tv, 1 - Math.exp(-ACCEL * dt));
    botPos.addScaledVector(record.vel, dt);

    // Hard collision resolution: push bot out of obstacles (moon) and asteroids
    // so they can't physically pass through them, regardless of steering.
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

    record.targetPos.copy(botPos);
    record.targetQuat.copy(record.ship.quaternion);
    record.hasTarget = true;

    // Stuck detector: if velocity stays low while we want to be moving,
    // jolt into evade with a random axis to break free.
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

    // Fire — projectile leaves along forward at BULLET_SPEED. Check whether
    // forward is pointed close enough to the wobble-adjusted lead point.
    if (state === 'attack' && fireTimer <= 0 && dist < FIRE_RANGE) {
      const ideal = aimWorld.clone().sub(botPos).normalize();
      if (tmpFwd.dot(ideal) > FIRE_DOT) {
        fireBullet();
        fireTimer = FIRE_COOLDOWN;
      }
    }
  }

  function fireBullet() {
    const botPos = record.ship.position;
    tmpFwd.set(0, 0, 1).applyQuaternion(record.ship.quaternion);
    const start = botPos.clone().addScaledVector(tmpFwd, 2.5);
    // Visual via shared bullet pool (renders + dies on asteroid contact).
    if (bullets) bullets.fire(start, tmpFwd, faction);
    // Multiplayer hook: lets the host broadcast bot-fire so non-host
    // clients render the same bullet visual.
    if (onFire) onFire(start, tmpFwd);
    // Authoritative tracking: each bot owns its in-flight bullets and runs
    // its own collision against opponents/asteroids. bullets.js only
    // resolves hits for the local player's own bullets, so without this
    // ledger bot bullets would be visual-only.
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

      // Hit any opponent? Sphere-overlap check.
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

      // Hit asteroid? Bullet dies (no damage to the rock).
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
      // Hit a static obstacle (moon)? Same outcome — bullet dies silently.
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
    record.vel.set(0, 0, 0);
  }

  return { update, notifyHit, notifyRespawn };
}
