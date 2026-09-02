import type { AuthFailureKind } from './auth-failure.js';

/**
 * The last known answer to "does this machine have a usable session?".
 *
 * **This never holds a token.** It holds a boolean and, when that boolean is
 * false, the reason. The credential itself stays inside `governance-auth`,
 * which is the whole point of ADR-0010; caching one here would recreate the
 * second credential path the extension exists to avoid.
 *
 * It exists because the answer was being recomputed by spawning a process.
 * `provideLanguageModelChatInformation` is called from several VS Code
 * contexts at once, and each call spawned `governance-auth token` — on top of
 * the spawn the catalogue fetch does. Measured before this file existed: five
 * concurrent picker queries produced five extra spawns purely to answer a
 * question whose answer had not changed.
 *
 * Staleness is safe in the only direction it can go wrong. A stale *positive*
 * does not authorize anything: every path that actually talks to the gateway
 * still calls `getToken`, which still spawns, so a session that lapsed inside
 * the TTL fails at the request rather than being let through. A stale
 * *negative* only withholds, which is the direction this repository always
 * takes, and it is cut short by the invalidations below.
 *
 * vscode-free on purpose, so the cache and its transitions are testable
 * outside an extension host.
 */

export interface AuthState {
  readonly signedIn: boolean;
  /** Why not, when `signedIn` is false. */
  readonly kind?: AuthFailureKind | undefined;
  /** `Date.now()` at which this was observed. */
  readonly at: number;
}

/**
 * How long an observation is reused.
 *
 * Short on purpose: the cache is a spawn-rate control, not a session store.
 * Thirty seconds collapses the burst of calls VS Code makes when a picker
 * opens while keeping the window in which a lapsed session still looks live
 * smaller than the time it takes a developer to notice.
 */
export const AUTH_STATE_TTL_MS = 30_000;

let current: AuthState | undefined;
let generation = 0;
const listeners = new Set<() => void>();

/**
 * A monotonic counter that steps whenever the state *meaningfully* changes.
 *
 * Callers use it to show a notification once per transition rather than once
 * per failed request — a developer who sends three messages while signed out
 * should get one toast, not three.
 */
export function authStateGeneration(): number {
  return generation;
}

/** The last observation, if it is still within `maxAgeMs`. */
export function cachedAuthState(maxAgeMs: number = AUTH_STATE_TTL_MS): AuthState | undefined {
  if (current === undefined || Date.now() - current.at >= maxAgeMs) {
    return undefined;
  }
  return current;
}

/**
 * Record what `governance-auth token` just did.
 *
 * Called from both branches of `getToken`, so every request the extension
 * already makes refreshes this for free and the probe below is only needed
 * when nothing has been observed at all.
 */
export function recordAuthOutcome(signedIn: boolean, kind?: AuthFailureKind): AuthState {
  const previous = current;
  current = { signedIn, kind, at: Date.now() };

  if (previous?.signedIn !== signedIn || previous?.kind !== kind) {
    generation++;
    for (const listener of listeners) {
      listener();
    }
  }
  return current;
}

/**
 * Forget the observation entirely.
 *
 * Used where the answer is known to have moved rather than merely aged: a
 * settings change (a different binary or gateway is a different question), and
 * the sign-in command (the developer just did the thing that fixes it, and
 * making them wait out a TTL to see that would be its own bad experience).
 */
export function invalidateAuthState(): void {
  if (current !== undefined) {
    current = undefined;
    generation++;
    for (const listener of listeners) {
      listener();
    }
  }
}

/** Subscribe to transitions. The returned handle is a `vscode.Disposable`. */
export function onAuthStateChange(listener: () => void): { dispose(): void } {
  listeners.add(listener);
  return {
    dispose(): void {
      listeners.delete(listener);
    },
  };
}
