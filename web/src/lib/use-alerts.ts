import { useCallback, useEffect, useRef, useState } from "react";

import type { AlertItem } from "../components/alert-stack";

function pruneAlerts(items: AlertItem[]): AlertItem[] {
  const autoClose = items.filter((a) => a.autoCloseMs != null);
  if (autoClose.length <= 3) return items;
  const keep = new Set(autoClose.slice(-3).map((a) => a.id));
  return items.filter((a) => a.autoCloseMs == null || keep.has(a.id));
}

export function useAlerts() {
  const [alerts, setAlerts] = useState<AlertItem[]>([]);
  const alertTimersRef = useRef<Record<string, number>>({});

  const closeAlert = useCallback((id: string) => {
    const timerId = alertTimersRef.current[id];
    if (timerId != null) {
      window.clearTimeout(timerId);
      delete alertTimersRef.current[id];
    }
    setAlerts((prev) => prev.filter((a) => a.id !== id));
  }, []);

  const pushAlert = useCallback((alert: AlertItem) => {
    setAlerts((prev) => pruneAlerts([...prev, alert]));
  }, []);

  const clearAlerts = useCallback(() => {
    for (const timerId of Object.values(alertTimersRef.current)) {
      window.clearTimeout(timerId);
    }
    alertTimersRef.current = {};
    setAlerts([]);
  }, []);

  useEffect(() => {
    const activeIds = new Set(alerts.map((a) => a.id));
    for (const [id, timerId] of Object.entries(alertTimersRef.current)) {
      if (activeIds.has(id)) continue;
      window.clearTimeout(timerId);
      delete alertTimersRef.current[id];
    }

    for (const a of alerts) {
      const autoCloseMs = a.autoCloseMs;
      if (autoCloseMs == null) continue;
      if (alertTimersRef.current[a.id] != null) continue;
      alertTimersRef.current[a.id] = window.setTimeout(() => {
        setAlerts((prev) => prev.filter((item) => item.id !== a.id));
      }, autoCloseMs);
    }
  }, [alerts]);

  useEffect(() => {
    return () => {
      for (const timerId of Object.values(alertTimersRef.current)) {
        window.clearTimeout(timerId);
      }
      alertTimersRef.current = {};
    };
  }, []);

  return { alerts, pushAlert, closeAlert, clearAlerts };
}

