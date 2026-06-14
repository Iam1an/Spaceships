# Spaceships

A multiplayer 3D space combat game built with Three.js and Node.js. Pilot your ship through asteroid fields, engage in dogfights with other players, customize your vessel, or battle through a single-player campaign.

## Features

- **3D Space Combat** - Fully realized 3D environments with asteroids, moons, and a dynamic skybox
- **Multiplayer** - Real-time multiplayer gameplay using WebSockets
- **Campaign Mode** - Three-mission solo campaign with a boss capital ship, checkpoints, and a lives system
- **Ship Customization** - Personalize your spaceship with different colors and styles
- **Weapons Arsenal** - Bullets, beam weapons, and homing missiles with flare countermeasures
- **Dynamic Physics** - Realistic flight mechanics with collision detection
- **Audio System** - Immersive sound effects for weapons, engines, and environmental audio
- **Mobile Support** - Touch-friendly HUD controls for mobile and tablet devices
- **Gamepad Support** - Full controller support via the Web Gamepad API
- **User Accounts** - Secure authentication with password encryption to track pilot stats
- **AI Opponents** - Bot pilots to challenge you when playing solo

## Play Now

The game is hosted and ready to play at **[gheat.net/spaceships](https://gheat.net/spaceships)**

No installation required - just open the link in your browser and start piloting!

## Project Structure

```
├── public/
│   └── src/                  # Client-side game code
│       ├── main.js           # Main game loop and initialization
│       ├── ship.js           # Ship model and mechanics
│       ├── asteroids.js      # Asteroid generation and physics
│       ├── bot.js            # AI bot pilots
│       ├── bullets.js        # Projectile system
│       ├── beams.js          # Beam weapons
│       ├── missiles.js       # Homing missiles and flare countermeasures
│       ├── warp.js           # Warp jump visual effect
│       ├── lobby.js          # Multiplayer lobby and matchmaking
│       ├── auth.js           # Client-side authentication
│       ├── customization.js  # Ship customization UI
│       ├── input.js          # Keyboard/mouse/gamepad controls
│       ├── touchhud.js       # Mobile touch controls
│       ├── camera.js         # Third-person camera system
│       ├── audio.js          # Sound effects and music
│       ├── filter.js         # Post-processing visual filters
│       ├── skybox.js         # Space background rendering
│       ├── trails.js         # Engine trail effects
│       ├── moon.js           # Moon object and rendering
│       ├── mothership.js     # Static mothership object
│       ├── carrier.js        # Aircraft carrier (team base)
│       ├── terrain.js        # Ground terrain and airfields
│       ├── airfield.js       # Airfield landing zones
│       ├── trees.js          # Environmental foliage
│       ├── water.js          # Water surface rendering
│       └── clouds.js         # Volumetric cloud layer
├── server/                   # Backend server code
│   ├── index.js              # Express server and WebSocket handler
│   └── db.js                 # SQLite database interface
├── public/                   # Static assets (images, sounds, etc.)
├── index.html                # Main HTML entry point
└── pilots.db                 # SQLite database (user accounts)
```

## Gameplay

### Controls

**Keyboard & Mouse:**
- **W / S** - Thrust forward / back
- **A / D** - Roll left / right
- **Mouse** - Aim and look around
- **Left Click** - Fire bullets
- **Right Click (hold)** - Free-look camera
- **F** - Fire homing missile
- **Q** - Deploy flares (missile countermeasure)
- **Shift** - Boost
- **Space** - Drift

**Gamepad:**
- **Left Stick** - Roll / Throttle
- **Right Stick** - Aim / Look
- **RT / A** - Fire
- **LT** - Drift
- **LB** - Boost

**Touch Controls:**
- Virtual joystick for movement
- Aim crosshair with finger
- On-screen buttons for weapons and boost

### Game Modes

- **Multiplayer** - Join lobbies and compete against other players in real-time
- **Solo** - Practice against AI bots or explore the asteroid field
- **Campaign** - Three-mission story mode with escalating difficulty and a boss fight
- **Customization** - Personalize your ship's appearance before battle

## Features in Detail

### Campaign Mode
Three sequential missions, each unlocked by completing the previous one. Fight through asteroid corridors packed with bot defenders, then take on a capital ship boss with four rotating turrets. You get three lives per run — dying warps you back to the last checkpoint at 55% health. Mission progress is saved locally.

### Homing Missiles & Flares
Lock on and fire a homing missile that navigates around asteroids to reach its target. Enemies (and you) can pop flares to seduce incoming missiles away. Each flare burst deploys 20 flares that burn for ~1.8 seconds.

### Ship Customization
Create a unique ship by selecting from various color schemes and visual styles. Your customization is saved to your pilot account.

### Physics System
Ships respond realistically to input with proper acceleration, momentum, and rotation. Collisions with asteroids and other ships have consequences.

### Weapons
- **Bullets** - Fast projectiles with limited range
- **Beams** - Sustained energy weapons with overheating
- **Missiles** - Homing projectiles with obstacle avoidance; countered by flares

### Audio
Dynamic audio system with effects for thrusters, weapon fire, impacts, and ambient space sounds.

### Multiplayer
Uses WebSocket connections for real-time synchronization of ship positions, rotations, and combat events across all connected players.

## Technology Stack

- **Frontend:**
  - Three.js - 3D graphics and rendering
  - HTML5 Canvas - Game rendering
  - JavaScript (ES6 modules)

- **Backend:**
  - Express.js - Web server
  - WebSocket (ws) - Real-time multiplayer communication
  - SQLite3 / better-sqlite3 - Data persistence
  - bcrypt - Secure password hashing
  - JWT - Authentication tokens

## Database

The game uses SQLite to store:
- Pilot accounts and login credentials
- Ship customization preferences
- Game stats and progression

Database file: `pilots.db`

## Development

### Key Components

- **main.js** - Game loop, rendering pipeline, campaign logic, and game state management
- **ship.js** - Ship movement, rotation, and physics
- **bot.js** - AI pathfinding and combat logic
- **missiles.js** - Homing missile guidance, obstacle avoidance, and flare system
- **lobby.js** - Multiplayer matchmaking, lobby system, and campaign mission routing
- **server/index.js** - Express routes, WebSocket message handling, game server logic

### Adding New Features

1. Client-side features go in `public/src/`
2. Server-side features and game logic in `server/`
3. Update the protocol in both locations to keep them in sync

## Performance

The game is optimized for:
- 60 FPS gameplay
- Efficient particle effects and trails
- Frustum culling for asteroids
- WebSocket message batching for multiplayer sync

## License

This game and all its code, assets, and design are the exclusive property of the creators. Unauthorized copying, reproduction, distribution, or use of this game in any form is strictly prohibited. Do not copy or steal this game.

## Credits

- **Ian** - Creator of the game
- **Gheat** - Online play and backend infrastructure

---

**Ready to pilot? Launch the game and dominate the asteroid field!**
