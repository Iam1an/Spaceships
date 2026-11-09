// Application entry point (see index.html). Everything the pre-match UI needs
// lives under ./lobby/; importing a module is what wires its screen up, so the
// order below is the order the lobby builds itself in.
import './lobby/screens.js';
import './lobby/pilot.js';
import './lobby/settings.js';
import './lobby/credits.js';
import './lobby/profile.js';
import './lobby/customize.js';
import './lobby/launch.js';
import './lobby/rooms.js';
import './lobby/solo.js';
import './lobby/gamepadnav.js';

import { requireAuth } from './auth.js';
import { lockCallsignToAccount } from './lobby/pilot.js';
import { refreshLogoutRow } from './lobby/settings.js';
import { refreshCredits } from './lobby/credits.js';
import { refreshUnlocks } from './lobby/unlocks.js';
import { checkPendingAchievements } from './lobby/profile.js';

// Nothing account-shaped can be filled in until we know who is flying.
requireAuth().then(() => {
  refreshLogoutRow();
  lockCallsignToAccount();
  refreshCredits();
  refreshUnlocks();
  checkPendingAchievements();
});

// Let the title animation finish before the menu becomes interactive.
setTimeout(() => document.body.classList.remove('intro-active'), 3500);
