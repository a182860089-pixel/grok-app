/**
 * Sidebar session catalog: list, fanout refresh, and multi-select.
 * Open/new-chat stay with the host. Persistence here is the host index, not prefs.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "@/lib/api";
import {
  mapSessionListRow,
  type SessionRow,
} from "@/lib/app/sidebarModels";
import { sortSessionsForSidebar } from "@/lib/sidebarDateGroups";
import {
  addIdsToSet,
  areAllIdsSelected,
  pruneSelectedIds,
  rangeIdsInclusive,
  toggleIdInSet,
  toggleIdsInSet,
} from "@/lib/sessionSelect";

export type CatalogProject = { id: string };

/** Visual order for Shift-range select: pinned, then projects, then orphans. Expand state ignored. */
export function sessionSidebarSelectOrder(
  sessions: readonly SessionRow[],
  projects: readonly CatalogProject[],
): string[] {
  const ids: string[] = [];
  const projectIdSet = new Set(projects.map((p) => p.id));
  const pinned = sessions.filter((s) => !!s.pinned && !s.archived);
  for (const s of sortSessionsForSidebar(pinned)) ids.push(s.id);
  for (const proj of projects) {
    const projSessions = sessions.filter(
      (s) => s.projectId === proj.id && !s.archived && !s.pinned,
    );
    for (const s of sortSessionsForSidebar(projSessions)) ids.push(s.id);
  }
  const orphans = sessions.filter(
    (s) =>
      (!s.projectId || !projectIdSet.has(s.projectId)) &&
      !s.archived &&
      !s.pinned,
  );
  for (const s of sortSessionsForSidebar(orphans)) ids.push(s.id);
  return ids;
}

export function useSessionCatalog(opts: {
  projects: readonly CatalogProject[];
  isDialogOpen: () => boolean;
}) {
  const optsRef = useRef(opts);
  optsRef.current = opts;

  const [sessions, setSessions] = useState<SessionRow[]>([]);
  const sessionsRef = useRef(sessions);
  sessionsRef.current = sessions;

  const [sessionSelectMode, setSessionSelectMode] = useState(false);
  const [selectedSessionIds, setSelectedSessionIds] = useState<Set<string>>(
    () => new Set(),
  );
  const sessionSelectAnchorIdRef = useRef<string | null>(null);
  const sidebarSelectOrderIdsRef = useRef<string[]>([]);

  const sidebarSelectOrderIds = useMemo(
    () => sessionSidebarSelectOrder(sessions, opts.projects),
    [sessions, opts.projects],
  );
  sidebarSelectOrderIdsRef.current = sidebarSelectOrderIds;

  const selectableSessionIds = useMemo(() => {
    const ids = new Set<string>();
    for (const s of sessions) {
      if (!s.archived) ids.add(s.id);
    }
    return ids;
  }, [sessions]);
  const selectableSessionCount = selectableSessionIds.size;

  useEffect(() => {
    setSelectedSessionIds((prev) => pruneSelectedIds(prev, selectableSessionIds));
    const anchor = sessionSelectAnchorIdRef.current;
    if (anchor && !selectableSessionIds.has(anchor)) {
      sessionSelectAnchorIdRef.current = null;
    }
  }, [selectableSessionIds]);

  useEffect(() => {
    if (!sessionSelectMode) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (optsRef.current.isDialogOpen()) return;
      e.preventDefault();
      setSessionSelectMode(false);
      setSelectedSessionIds(new Set());
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [sessionSelectMode]);

  const exitSessionSelectMode = useCallback(() => {
    setSessionSelectMode(false);
    setSelectedSessionIds(new Set());
    sessionSelectAnchorIdRef.current = null;
  }, []);

  const enterSessionSelectMode = useCallback((preselectId?: string) => {
    setSessionSelectMode(true);
    setSelectedSessionIds(preselectId ? new Set([preselectId]) : new Set());
    sessionSelectAnchorIdRef.current = preselectId ?? null;
  }, []);

  const toggleSessionSelected = useCallback(
    (id: string, toggleOpts?: { shiftKey?: boolean }) => {
      setSessionSelectMode(true);
      if (toggleOpts?.shiftKey) {
        const anchor = sessionSelectAnchorIdRef.current;
        if (anchor) {
          const range = rangeIdsInclusive(
            sidebarSelectOrderIdsRef.current,
            anchor,
            id,
          );
          if (range.length > 0) {
            setSelectedSessionIds((prev) => addIdsToSet(prev, range));
            return;
          }
        }
        setSelectedSessionIds((prev) => addIdsToSet(prev, [id]));
        sessionSelectAnchorIdRef.current = id;
        return;
      }
      setSelectedSessionIds((prev) => toggleIdInSet(prev, id));
      sessionSelectAnchorIdRef.current = id;
    },
    [],
  );

  const toggleSessionsSelected = useCallback((ids: readonly string[]) => {
    if (ids.length === 0) return;
    setSelectedSessionIds((prev) => {
      const next = toggleIdsInSet(prev, ids);
      if (areAllIdsSelected(next, ids)) {
        sessionSelectAnchorIdRef.current = ids[ids.length - 1] ?? null;
      }
      return next;
    });
  }, []);

  const refreshSessions = useCallback(async () => {
    try {
      const list = await api.sessionsList();
      setSessions(list.map((s) => mapSessionListRow(s)));
      void api.trayRefresh();
    } catch {
      /* ignore */
    }
  }, []);

  const refreshSessionsRef = useRef(refreshSessions);
  refreshSessionsRef.current = refreshSessions;

  useEffect(() => {
    if (!api.hasHost()) return;
    let cancelled = false;
    let timer: number | null = null;
    let unlisten: (() => void) | undefined;
    void (async () => {
      const un = await api.listen<{ reason?: string; sessionId?: string }>(
        "sessions://changed",
        () => {
          if (cancelled) return;
          if (timer !== null) window.clearTimeout(timer);
          timer = window.setTimeout(() => {
            timer = null;
            void refreshSessionsRef.current();
          }, 150);
        },
      );
      if (cancelled) un();
      else unlisten = un;
    })();
    return () => {
      cancelled = true;
      if (timer !== null) window.clearTimeout(timer);
      unlisten?.();
    };
  }, []);

  return {
    sessions,
    setSessions,
    sessionsRef,
    refreshSessions,
    refreshSessionsRef,
    sessionSelectMode,
    selectedSessionIds,
    selectableSessionCount,
    enterSessionSelectMode,
    exitSessionSelectMode,
    toggleSessionSelected,
    toggleSessionsSelected,
  };
}
