import { describe, expect, it } from "vitest";
import {
  liveDragWidth,
  project,
  rubberband,
  snapAfterFlick,
  springSettled,
} from "./motionSpring";

describe("motionSpring", () => {
  it("projects farther with higher velocity (Apple decay form)", () => {
    const slow = project(200);
    const fast = project(800);
    expect(fast).toBeGreaterThan(slow);
    expect(project(0)).toBe(0);
    expect(project(Number.NaN)).toBe(0);
  });

  it("rubberbands less than the raw overshoot", () => {
    expect(rubberband(100, 200)).toBeLessThan(100);
    expect(rubberband(100, 200)).toBeGreaterThan(0);
    expect(rubberband(-80, 200)).toBeLessThan(0);
    expect(rubberband(0, 200)).toBe(0);
  });

  it("tracks 1:1 inside bounds and resists past max", () => {
    expect(liveDragWidth(240, 200, 420)).toBe(240);
    expect(liveDragWidth(500, 200, 420)).toBeGreaterThan(420);
    expect(liveDragWidth(500, 200, 420)).toBeLessThan(500);
    expect(liveDragWidth(50, 200, 420)).toBe(200);
    expect(liveDragWidth(50, 200, 420, { bandMin: true })).toBeLessThan(200);
  });

  it("snaps a flick to the projected landing, not the release point", () => {
    const snaps = [0, 268];
    expect(snapAfterFlick(200, 4000, snaps)).toBe(268);
    expect(snapAfterFlick(80, -4000, snaps)).toBe(0);
  });

  it("treats near-target + low velocity as settled", () => {
    expect(springSettled(268, 268, 0)).toBe(true);
    expect(springSettled(10, 268, 400)).toBe(false);
  });
});
