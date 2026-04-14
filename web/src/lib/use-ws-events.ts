import { useEffect, useRef } from "react";

import { wsUrl } from "./api";

export function useWsEvents(opts: {
  onEvent: (ev: any) => void;
  isSessionCurrent: (sessionId: string) => boolean;
  getAfterEventId: () => number;
}) {
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectAttemptRef = useRef(0);
  const reconnectTimerRef = useRef<number | null>(null);

  const onEventRef = useRef(opts.onEvent);
  const isSessionCurrentRef = useRef(opts.isSessionCurrent);
  const getAfterEventIdRef = useRef(opts.getAfterEventId);

  useEffect(() => {
    onEventRef.current = opts.onEvent;
  }, [opts.onEvent]);
  useEffect(() => {
    isSessionCurrentRef.current = opts.isSessionCurrent;
  }, [opts.isSessionCurrent]);
  useEffect(() => {
    getAfterEventIdRef.current = opts.getAfterEventId;
  }, [opts.getAfterEventId]);

  function clearReconnectTimer() {
    if (reconnectTimerRef.current != null) {
      window.clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
  }

  function close() {
    clearReconnectTimer();
    if (wsRef.current) {
      wsRef.current.close();
      wsRef.current = null;
    }
  }

  function resetReconnectAttempt() {
    reconnectAttemptRef.current = 0;
  }

  function scheduleReconnect(sessionId: string) {
    if (reconnectTimerRef.current != null) return;
    const attempt = reconnectAttemptRef.current;
    const delay = Math.min(10_000, 250 * Math.pow(2, attempt));
    reconnectAttemptRef.current = Math.min(attempt + 1, 10);

    reconnectTimerRef.current = window.setTimeout(() => {
      reconnectTimerRef.current = null;
      if (!isSessionCurrentRef.current(sessionId)) return;
      connect(sessionId, getAfterEventIdRef.current());
    }, delay);
  }

  function connect(sessionId: string, afterEventId: number) {
    clearReconnectTimer();
    if (wsRef.current) {
      wsRef.current.close();
      wsRef.current = null;
    }

    const url = wsUrl(
      `/v1/sessions/${sessionId}/events/ws?after_event_id=${afterEventId}`,
    );
    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onopen = () => {
      reconnectAttemptRef.current = 0;
    };
    ws.onmessage = (ev) => {
      try {
        const msg = JSON.parse(ev.data as string);
        onEventRef.current(msg);
      } catch (e) {
        // eslint-disable-next-line no-console
        console.error("ws parse error", e);
      }
    };
    ws.onerror = () => {
      // eslint-disable-next-line no-console
      console.error("ws error");
    };
    ws.onclose = () => {
      if (wsRef.current !== ws) return;
      wsRef.current = null;
      scheduleReconnect(sessionId);
    };
  }

  useEffect(() => {
    return () => close();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { connect, close, resetReconnectAttempt };
}

