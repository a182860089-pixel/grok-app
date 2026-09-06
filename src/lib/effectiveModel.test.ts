import { describe, expect, it } from "vitest";
import {
  composerModelChipLabel,
  customRouteChipModel,
  effectiveComposerModel,
} from "./effectiveModel";

describe("effectiveComposerModel", () => {
  it("keeps the official catalog selection on the official route", () => {
    expect(effectiveComposerModel("grok-4.5", null)).toBe("grok-4.5");
    expect(effectiveComposerModel("grok-3", undefined)).toBe("grok-3");
  });

  it("prefers the active custom provider request model", () => {
    expect(effectiveComposerModel("grok-4.5", "deepseek-v4-flash")).toBe(
      "deepseek-v4-flash",
    );
  });

  it("falls back to the composer selection when provider model is blank", () => {
    expect(effectiveComposerModel("grok-4.5", "")).toBe("grok-4.5");
    expect(effectiveComposerModel("grok-4.5", "   ")).toBe("grok-4.5");
  });

  it("keeps the composer selection when no provider is active", () => {
    expect(effectiveComposerModel("grok-4.5", "")).toBe("grok-4.5");
  });
});

describe("composerModelChipLabel", () => {
  it("uses official label when no custom route", () => {
    expect(
      composerModelChipLabel({
        modelId: "grok-4.5",
        officialLabel: "Grok 4.5",
        activeCustom: null,
      }),
    ).toBe("Grok 4.5");
  });

  it("uses custom name when set", () => {
    expect(
      composerModelChipLabel({
        modelId: "grok-4.5",
        officialLabel: "Grok 4.5",
        activeCustom: { name: "云驿 DeepSeek", model: "deepseek-chat" },
      }),
    ).toBe("云驿 DeepSeek");
  });

  it("falls back to custom model when name empty", () => {
    expect(
      composerModelChipLabel({
        modelId: "grok-4.5",
        officialLabel: "Grok 4.5",
        activeCustom: { name: "  ", model: "deepseek-chat" },
      }),
    ).toBe("deepseek-chat");
  });

  it("uses the session model, not the provider default", () => {
    expect(
      composerModelChipLabel({
        modelId: "gpt-5.6-sol",
        officialLabel: "Grok 4.6",
        activeCustom: customRouteChipModel({
          provider: {
            name: "gpt",
            model: "gpt-6-astra",
            models: [
              { id: "gpt-5.6-sol", name: "gpt-5.6-sol" },
              { id: "gpt-6-astra", name: "gpt-6-astra" },
            ],
          },
          sessionModelId: "gpt-5.6-sol",
        }),
      }),
    ).toBe("gpt-5.6-sol");
  });
});
