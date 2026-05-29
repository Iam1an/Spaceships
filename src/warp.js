import * as THREE from 'three';

export function createWarpEffect(scene, camera) {
  const WARP_DURATION = 2.5; // seconds
  let warpTimer = WARP_DURATION;

  const starCount = 800;
  const lineGeo = new THREE.BufferGeometry();
  const linePos = new Float32Array(starCount * 6); // 2 vertices per line segment
  const velocities = new Float32Array(starCount);

  for (let i = 0; i < starCount; i++) {
    const angle = Math.random() * Math.PI * 2;
    const radius = 25 + Math.random() * 150;
    
    const x = Math.cos(angle) * radius;
    const y = Math.sin(angle) * radius;
    const z = (Math.random() - 0.5) * 800; // Initial Z spread
    
    linePos[i * 6 + 0] = x;
    linePos[i * 6 + 1] = y;
    linePos[i * 6 + 2] = z;
    
    linePos[i * 6 + 3] = x;
    linePos[i * 6 + 4] = y;
    linePos[i * 6 + 5] = z + 20; // Length
    
    velocities[i] = 1500 + Math.random() * 1500;
  }
  
  lineGeo.setAttribute('position', new THREE.BufferAttribute(linePos, 3));

  const material = new THREE.LineBasicMaterial({
    color: 0xaaddff,
    transparent: true,
    opacity: 1.0,
    blending: THREE.AdditiveBlending,
    depthWrite: false
  });

  const lines = new THREE.LineSegments(lineGeo, material);
  scene.add(lines);

  const baseFov = camera.fov;
  const maxFov = 115;
  camera.fov = maxFov;
  camera.updateProjectionMatrix();

  return {
    update: (dt) => {
      if (warpTimer <= 0) {
        if (lines.visible) {
          lines.visible = false;
          camera.fov = baseFov;
          camera.updateProjectionMatrix();
        }
        return;
      }

      warpTimer -= dt;
      let progress = 1.0 - (warpTimer / WARP_DURATION);
      if (progress < 0) progress = 0;
      if (progress > 1) progress = 1;

      // Opacity fades out heavily towards the end
      const opacity = warpTimer < 0.6 ? (warpTimer / 0.6) : 1.0;
      material.opacity = opacity;

      // Smoothly reduce FOV
      // fovProgress goes from 0 to 1 with an ease-out curve
      const fovProgress = 1.0 - Math.pow(1.0 - progress, 3);
      camera.fov = THREE.MathUtils.lerp(maxFov, baseFov, fovProgress);
      camera.updateProjectionMatrix();

      // Lock stars to camera position and orientation
      lines.position.copy(camera.position);
      lines.quaternion.copy(camera.quaternion);

      const posAttr = lineGeo.attributes.position;
      const arr = posAttr.array;
      
      // Speed multiplier slows down as we exit warp
      const speedMult = THREE.MathUtils.lerp(2.5, 0.1, progress);

      for (let i = 0; i < starCount; i++) {
        let z1 = arr[i * 6 + 2];
        let z2 = arr[i * 6 + 5];

        const speed = velocities[i] * dt * speedMult;
        z1 += speed;
        
        // Wrap stars from behind the camera back to far front
        if (z1 > 50) {
          z1 -= 800;
        }

        arr[i * 6 + 2] = z1;
        arr[i * 6 + 5] = z1 + speed * 0.15 + 10; // Dynamic stretching
      }
      posAttr.needsUpdate = true;
    }
  };
}
