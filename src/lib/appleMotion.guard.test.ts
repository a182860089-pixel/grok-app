/**
 * Apple Design contract locks — press + reduced-transparency tokens.
 * Spec: docs/llm-wiki/apple-motion.md
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const tokens = readFileSync(resolve(__dirname, "../styles/tokens.css"), "utf8");
const libRs = readFileSync(resolve(__dirname, "../../src-tauri/src/lib.rs"), "utf8");
const wiki = readFileSync(
  resolve(__dirname, "../../docs/llm-wiki/apple-motion.md"),
  "utf8",
);

describe("apple-motion contract", () => {
  it("ships press tokens and pointer-down scale on buttons/chips/perm-bar", () => {
    expect(tokens).toContain("--press-scale: 0.88");
    expect(tokens).toContain("--press-ms: 80ms");
    expect(tokens).toContain(".btn:active:not(:disabled)");
    expect(tokens).toContain(".chip:active:not(:disabled)");
    expect(tokens).toContain(".perm-bar__btn:active:not(:disabled)");
  });

  it("falls back to solid glass when the OS asks for reduced transparency", () => {
    expect(tokens).toContain("prefers-reduced-transparency: reduce");
    expect(tokens).toContain("--glass-surface: var(--glass-surface-solid)");
    expect(tokens).toContain("--glass-blur: 0px");
    expect(tokens).toContain("--bg-sidebar: var(--bg-sidebar-solid)");
  });

  it("uses size-specific tracking and solid Win/Linux sidebar material", () => {
    expect(tokens).toContain("--track-display: -0.02em");
    expect(tokens).toContain("--track-ui: 0.01em");
    expect(tokens).toContain(".platform-win");
    expect(tokens).toMatch(/\.platform-win[\s\S]*--sidebar-blur: 0px/);
  });

  it("does not auto-start Remote IM unless should_boot_at_launch", () => {
    expect(libRs).toContain("should_boot_at_launch");
    expect(libRs).toContain("skip boot autostart and watchdog");
    expect(wiki).toContain("enabled: false");
  });
});
