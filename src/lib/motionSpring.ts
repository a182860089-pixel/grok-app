/**
 * Apple Design fluid-interface math (WWDC 2018) for the four gesture panes.
 * See docs/llm-wiki/apple-motion.md.
 *
 * Default UI is critically damped (no bounce). Overshoot only when a drag
 * is already past a bound (rubber-band) or a flick carries momentum.
 */

/** Apple projection: exponential decay, not v²/(2a). */
export function project(
  initialVelocityPxPerSec: number,
  decelerationRate = 0.998,
): number {
  if (!Number.isFinite(initialVelocityPxPerSec)) return 0;
  const d = decelerationRate;
  if (!(d > 0) || !(d < 1)) return 0;
  return (initialVelocityPxPerSec / 1000) * d / (1 - d);
}

/** Progressive resistance past a bound. `dimension` is the pane size. */
export function rubberband(
  overshoot: number,
  dimension: number,
  constant = 0.9,
): number {
  if (!Number.isFinite(overshoot) || overshoot === 0) return 0;
  const dim = Number.isFinite(dimension) && dimension > 0 ? dimension : 1;
  const c = Number.isFinite(constant) && constant > 0 ? constant : 0.9;
  const sign = overshoot < 0 ? -1 : 1;
  const mag = Math.abs(overshoot);
  return (sign * mag * dim * c) / (dim + c * mag);
}

/**
 * Live drag sample: 1:1 inside [min, max], rubber-band past max (and min
 * when `bandMin` is true). Release must still snap with a hard clamp.
 */
export function liveDragWidth(
  desired: number,
  min: number,
  max: number,
  opts?: { bandMin?: boolean },
): number {
  if (!Number.isFinite(desired)) return min;
  const lo = Number.isFinite(min) ? min : 0;
  const hi = Number.isFinite(max) ? max : lo;
  if (hi < lo) return Math.max(0, hi);
  if (desired <= hi && desired >= lo) return desired;
  const dim = Math.max(1, hi - lo);
  if (desired > hi) return hi + rubberband(desired - hi, dim);
  if (opts?.bandMin) return lo - rubberband(lo - desired, dim);
  return lo;
}

/** Nearest snap after projecting the release velocity. */
export function snapAfterFlick(
  current: number,
  velocityPxPerSec: number,
  snaps: readonly number[],
  decelerationRate = 0.998,
): number {
  if (!snaps.length) return current;
  const projected = current + project(velocityPxPerSec, decelerationRate);
  let best = snaps[0];
  let bestDist = Math.abs(projected - best);
  for (let i = 1; i < snaps.length; i++) {
    const d = Math.abs(projected - snaps[i]);
    if (d < bestDist) {
      best = snaps[i];
      bestDist = d;
    }
  }
  return best;
}

/** Critically damped default: damping 1.0, response ~0.4s. */
export const SPRING_UI = { damping: 1, response: 0.4 } as const;
/** Flick / sheet: slight bounce. */
export const SPRING_FLICK = { damping: 0.8, response: 0.3 } as const;

export function springSettled(
  current: number,
  target: number,
  velocity = 0,
  epsilon = 0.5,
): boolean {
  return Math.abs(current - target) < epsilon && Math.abs(velocity) < 8;
}
