/**
 * Effective inference model shown in composer model chips.
 *
 * A custom provider (relay) is a **channel**: when it is the active route the
 * agent spawns with the provider's request model (`[model.<id>] model = …`)
 * and the official composer selection is ignored (`agent_spawn_model_id` in
 * providers.rs). The chip must therefore reflect the provider request model —
 * not the stale official catalog pick (default "Grok 4.5") that misleads users
 * into thinking the relay sends Grok.
 */

/**
 * Resolve the model id the composer chip should display.
 *
 * @param modelId Official catalog selection (composer state).
 * @param activeCustomModel Request model of the active custom provider, or
 *   null/undefined when the official route is active.
 */
export function effectiveComposerModel(
  modelId: string,
  activeCustomModel: string | null | undefined,
): string {
  const custom = activeCustomModel?.trim();
  return custom ? custom : modelId;
}

/**
 * Label for the composer model chip.
 *
 * Official route: catalog label (or model id).
 * Custom route: selected catalog entry name, falling back to request `model`.
 */
export function composerModelChipLabel(opts: {
  modelId: string;
  officialLabel: string;
  activeCustom: { name: string; model: string } | null | undefined;
}): string {
  const custom = opts.activeCustom;
  if (custom) {
    const name = custom.name?.trim();
    if (name) return name;
    const model = custom.model?.trim();
    if (model) return model;
  }
  return opts.officialLabel || opts.modelId;
}

/** Resolve the custom-route chip to the session's selected model, not the provider default. */
export function customRouteChipModel(opts: {
  provider:
    | {
        name: string;
        model: string;
        models?: Array<{ id: string; name: string }>;
      }
    | null
    | undefined;
  sessionModelId: string;
}): { name: string; model: string } | null {
  const p = opts.provider;
  if (!p) return null;
  const activeId = (opts.sessionModelId.trim() || p.model.trim());
  if (!activeId) {
    const name = p.name.trim();
    const model = p.model.trim();
    if (!name && !model) return null;
    return { name: name || model, model };
  }
  const entry = p.models?.find((m) => m.id === activeId);
  if (entry) {
    return { name: entry.name || entry.id, model: entry.id };
  }
  return { name: activeId, model: activeId };
}
